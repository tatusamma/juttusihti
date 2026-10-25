//! End-to-end runs against a scripted local server that imitates the
//! OpenAI, Anthropic and Gemini wire formats. No real provider is contacted
//! and nothing here costs money.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use juttusihti_ai::{Approval, Client, Control, Endpoint, Progress, RetryPolicy, RunError, Runner};
use juttusihti_core::analysis::{AnalysisConfig, Depth, Task};
use juttusihti_core::budget::Limits;
use juttusihti_core::estimate::estimate;
use juttusihti_core::pricing::{Catalog, Provider};
use juttusihti_core::search::SearchQuery;
use juttusihti_core::store::Store;
use juttusihti_core::{NewArticle, NewFeed, SessionStatus, Status};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const API_KEY: &str = "test-key-123";

#[derive(Debug, Clone)]
struct Recorded {
    path: String,
    headers: String,
    body: Value,
}

#[derive(Clone)]
struct MockReply {
    status: u16,
    body: Value,
    delay: Duration,
}

type Handler = Arc<dyn Fn(&Recorded, usize) -> MockReply + Send + Sync>;

struct Mock {
    base: String,
    seen: Arc<Mutex<Vec<Recorded>>>,
}

impl Mock {
    fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
    fn requests(&self) -> Vec<Recorded> {
        self.seen.lock().unwrap().clone()
    }
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 16384];
    loop {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_string();
            let len: usize = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))
                })
                .unwrap_or(0);
            while buf.len() < pos + 4 + len {
                let n = sock.read(&mut tmp).await.ok()?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let path = head.lines().next()?.split_whitespace().nth(1)?.to_string();
            return Some((path, head, buf[pos + 4..].to_vec()));
        }
    }
}

async fn mock(handler: Handler) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        let counter = Arc::new(AtomicUsize::new(0));
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let (log, handler, counter) = (log.clone(), handler.clone(), counter.clone());
            tokio::spawn(async move {
                let Some((path, headers, body)) = read_request(&mut sock).await else { return };
                let rec = Recorded { path, headers, body: serde_json::from_slice(&body).unwrap_or(Value::Null) };
                let n = counter.fetch_add(1, Ordering::SeqCst);
                log.lock().unwrap().push(rec.clone());
                let reply = handler(&rec, n);
                tokio::time::sleep(reply.delay).await;
                let body = reply.body.to_string();
                let head = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    reply.status,
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Mock { base, seen }
}

// ---- provider envelopes --------------------------------------------------------------

fn user_text(provider: Provider, body: &Value) -> String {
    match provider {
        Provider::Anthropic => body["messages"][0]["content"].as_str(),
        Provider::Openai => body["input"][1]["content"].as_str(),
        Provider::Gemini => body["contents"][0]["parts"][0]["text"].as_str(),
    }
    .unwrap_or_default()
    .to_string()
}

fn articles_in(text: &str) -> Vec<Value> {
    let start = text.find("<articles>\n").map(|i| i + "<articles>\n".len()).unwrap_or(0);
    let end = text.rfind("\n</articles>").unwrap_or(text.len());
    serde_json::from_str::<Value>(&text[start..end]).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}

fn envelope(provider: Provider, payload: &Value, input: u64, output: u64) -> Value {
    let text = payload.to_string();
    match provider {
        Provider::Anthropic => json!({
            "content": [{"type": "text", "text": text}], "stop_reason": "end_turn",
            "usage": {"input_tokens": input, "output_tokens": output, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
        }),
        Provider::Openai => json!({
            "status": "completed",
            "output": [{"type": "message", "content": [{"type": "output_text", "text": text}]}],
            "usage": {"input_tokens": input, "output_tokens": output, "output_tokens_details": {"reasoning_tokens": output / 2}}
        }),
        Provider::Gemini => json!({
            "candidates": [{"content": {"parts": [{"text": text}]}, "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": input, "candidatesTokenCount": output, "thoughtsTokenCount": 0}
        }),
    }
}

fn ok(body: Value) -> MockReply {
    MockReply { status: 200, body, delay: Duration::ZERO }
}

/// A well-behaved fake model: answers whatever task the request asks for.
fn good_model(provider: Provider) -> impl Fn(&Recorded, usize) -> MockReply + Send + Sync {
    move |r, _| {
        let schema_kind = match provider {
            Provider::Anthropic => r.body["output_config"]["format"]["schema"]["properties"].clone(),
            Provider::Openai => r.body["text"]["format"]["schema"]["properties"].clone(),
            Provider::Gemini => r.body["generationConfig"]["responseJsonSchema"]["properties"].clone(),
        };
        let user = user_text(provider, &r.body);
        let items = articles_in(&user);
        let payload = if schema_kind.get("articles").is_some() {
            json!({"articles": items.iter().map(|a| {
                let title = a["title"].as_str().unwrap_or_default();
                let first = title.split_whitespace().next().unwrap_or("x");
                json!({
                    "id": a["id"],
                    "labels": ["Politiikka"],
                    "entities": [{"name": first, "kind": "organization", "role": "subject", "evidence": first}],
                    "relevance": {"class": "relevant", "reason": "On topic."}
                })
            }).collect::<Vec<_>>()})
        } else if schema_kind.get("pairs").is_some() {
            json!({"pairs": items.iter().map(|p| json!({
                "a": p["a"]["id"], "b": p["b"]["id"], "relation": "follow_up", "evidence": "työttömyysturva", "confidence": 0.8
            })).collect::<Vec<_>>()})
        } else {
            let id = items.first().and_then(|a| a["id"].as_i64()).unwrap_or(0);
            json!({"summary": "Hallitus esitti muutoksia.", "key_points": [{"text": "Esitys", "article_ids": [id]}], "open_questions": []})
        };
        ok(envelope(provider, &payload, 1_000, 200))
    }
}

// ---- fixtures -----------------------------------------------------------------------

const OCT1: i64 = 1_790_812_800;
const DAY: i64 = 86_400;

fn model_for(p: Provider) -> &'static str {
    match p {
        Provider::Anthropic => "claude-haiku-5-5",
        Provider::Openai => "gpt-6-luna",
        Provider::Gemini => "gemini-3.8-flash",
    }
}

fn seed(n: usize) -> Arc<Mutex<Store>> {
    let mut s = Store::open_in_memory().unwrap();
    let f =
        s.add_feed(&NewFeed { url: "https://f.example/rss".into(), title: "F".into(), ..Default::default() }).unwrap();
    let mut entries: Vec<NewArticle> = (0..n)
        .map(|i| NewArticle {
            guid: format!("g{i}"),
            url: Some(format!("https://f.example/{i}")),
            title: format!("Uutinen {i} kertoo säästä"),
            summary: Some(format!("Kuvaus numero {i}.")),
            content: None,
            author: None,
            published_at: Some(OCT1 + i as i64 * 600),
        })
        .collect();
    // Two articles that the local candidate generator pairs up.
    entries.push(NewArticle {
        guid: "p1".into(),
        url: Some("https://f.example/p1".into()),
        title: "Hallitus esittää muutoksia työttömyysturvaan".into(),
        summary: Some("Esitys koskee päivärahaa.".into()),
        content: None,
        author: None,
        published_at: Some(OCT1),
    });
    entries.push(NewArticle {
        guid: "p2".into(),
        url: Some("https://f.example/p2".into()),
        title: "Hallituksen esitys työttömyysturvasta herättää kritiikkiä".into(),
        summary: Some("Oppositio arvostelee.".into()),
        content: None,
        author: None,
        published_at: Some(OCT1 + DAY),
    });
    s.ingest(f, &entries).unwrap();
    Arc::new(Mutex::new(s))
}

fn config(provider: Provider, tasks: &[Task]) -> AnalysisConfig {
    AnalysisConfig {
        scope: SearchQuery { from: Some(OCT1 - DAY), to: Some(OCT1 + 30 * DAY), ..Default::default() },
        range_label: "test".into(),
        provider,
        model: model_for(provider).into(),
        depth: Depth::HeadlinesAndDescriptions,
        tasks: tasks.iter().copied().collect::<BTreeSet<_>>(),
        limits: Limits { max_tokens: Some(10_000_000), max_cost: Some(10_000_000), ..Default::default() },
        max_articles: 1_000,
        reuse_cached: true,
        use_batch: false,
        relevance_criteria: Some("työttömyysturva".into()),
    }
}

fn session(db: &Arc<Mutex<Store>>, cfg: &AnalysisConfig) -> i64 {
    let mut s = db.lock().unwrap();
    let scope = s.analysis_scope(cfg).unwrap();
    let e = estimate(cfg, &scope, 0, &Catalog::builtin(), "2026-10-25".parse().unwrap());
    s.save_estimated_session(cfg, &scope, &e).unwrap()
}

fn fast() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 3,
        initial_backoff: Duration::from_millis(5),
        max_backoff: Duration::from_millis(20),
        timeout: Duration::from_secs(10),
    }
}

fn runner(db: &Arc<Mutex<Store>>, provider: Provider, base: &str, control: Control) -> Runner {
    let client = Client::new(Endpoint { provider, base_url: base.into(), api_key: API_KEY.into() }, fast()).unwrap();
    Runner::new(db.clone(), client, Catalog::builtin(), control)
}

const YES: Approval = Approval { consent_to_send: true };

fn ledger_rows(db: &Arc<Mutex<Store>>) -> Vec<(String, Option<i64>)> {
    let s = db.lock().unwrap();
    let mut stmt = s.conn().prepare("SELECT operation, cost_microunits FROM usage_ledger ORDER BY id").unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
}

// ---- tests ---------------------------------------------------------------------------

#[tokio::test]
async fn full_run_on_every_provider() {
    for provider in Provider::ALL {
        let db = seed(3);
        let cfg = config(provider, &[Task::Labels, Task::Entities, Task::Relevance, Task::Related, Task::Summary]);
        let sid = session(&db, &cfg);
        let m = mock(Arc::new(good_model(provider))).await;
        let summary = runner(&db, provider, &m.base, Control::default()).run(sid, YES).await.unwrap();
        assert_eq!(summary.final_status, Some(SessionStatus::Completed), "{provider:?}");
        assert_eq!(m.count(), 3, "{provider:?}: extraction, related, summary");
        assert!(summary.cost.unwrap() > 0, "{provider:?} priced from the catalog");

        // Requests carry auth in headers, never the body; OpenAI is told not to store.
        for r in m.requests() {
            assert!(r.headers.to_lowercase().contains(&API_KEY.to_lowercase()), "{provider:?} auth header");
            assert!(!r.body.to_string().contains(API_KEY));
            match provider {
                Provider::Openai => assert_eq!(r.body["store"], false),
                Provider::Anthropic => assert!(r.headers.contains("anthropic-version: 2026-10-21")),
                Provider::Gemini => assert!(r.path.starts_with("/v1beta/models/gemini-3.8-flash:generateContent")),
            }
        }

        let s = db.lock().unwrap();
        let rec = s.session(sid).unwrap();
        assert_eq!(rec.status, SessionStatus::Completed);
        // AI labels are suggestions only.
        let ids = s.session_articles(sid).unwrap();
        let labels = s.article_labels(ids[0]).unwrap();
        assert_eq!(labels[0].label.name, "Politiikka");
        assert_eq!(labels[0].status, Status::Suggested);
        let rels = s.relationships(ids[ids.len() - 1]).unwrap();
        assert!(rels.iter().all(|r| r.status == Status::Suggested && r.origin == juttusihti_core::Origin::Ai));
        assert_eq!(rels.len(), 1, "{provider:?}");
        let ledger: i64 =
            s.conn().query_row("SELECT count(*) FROM usage_ledger WHERE session_id = ?1", [sid], |r| r.get(0)).unwrap();
        assert_eq!(ledger, 3);
        let summaries: i64 =
            s.conn().query_row("SELECT count(*) FROM ai_suggestions WHERE kind = 'summary'", [], |r| r.get(0)).unwrap();
        assert_eq!(summaries, 1);
        let counts = s.review_counts(sid).unwrap();
        assert_eq!(counts.relevance, 5, "{provider:?}: one relevance suggestion per distinct article");
        // 3 pairs among the similar "Uutinen N kertoo säästä" headlines + the government pair.
        assert!(counts.entities > 0 && counts.relationships == 4 && counts.has_summary, "{provider:?} {counts:?}");
    }
}

#[tokio::test]
async fn resumes_without_repeating_finished_batches() {
    let db = seed(30); // 32 distinct articles → 2 extraction batches
    let cfg = config(Provider::Anthropic, &[Task::Labels]);
    let sid = session(&db, &cfg);
    let good = good_model(Provider::Anthropic);
    let failing = mock(Arc::new(move |r: &Recorded, n: usize| {
        if n == 0 {
            good(r, n)
        } else {
            MockReply { status: 400, body: json!({"error": {"message": "bad request"}}), delay: Duration::ZERO }
        }
    }))
    .await;
    let first = runner(&db, Provider::Anthropic, &failing.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(first.final_status, Some(SessionStatus::Failed));
    assert_eq!(failing.count(), 2, "400 is not retried");

    let m = mock(Arc::new(good_model(Provider::Anthropic))).await;
    let second = runner(&db, Provider::Anthropic, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(second.final_status, Some(SessionStatus::Completed));
    assert_eq!(m.count(), 1, "only the unfinished batch is sent again");
    let sent = articles_in(&user_text(Provider::Anthropic, &m.requests()[0].body));
    assert_eq!(sent.len(), 12);
}

#[tokio::test]
async fn cached_results_are_reused_by_later_sessions() {
    let db = seed(3);
    let cfg = config(Provider::Gemini, &[Task::Labels, Task::Entities]);
    let m = mock(Arc::new(good_model(Provider::Gemini))).await;
    let s1 = session(&db, &cfg);
    runner(&db, Provider::Gemini, &m.base, Control::default()).run(s1, YES).await.unwrap();
    assert_eq!(m.count(), 1);
    let s2 = session(&db, &cfg);
    let r = runner(&db, Provider::Gemini, &m.base, Control::default()).run(s2, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    assert_eq!(m.count(), 1, "no new provider calls");
    assert_eq!(r.calls, 0);
}

#[tokio::test]
async fn budget_limits_stop_before_sending() {
    let db = seed(3);
    let mut cfg = config(Provider::Openai, &[Task::Labels]);
    cfg.limits.max_tokens = Some(100);
    let sid = session(&db, &cfg);
    let m = mock(Arc::new(good_model(Provider::Openai))).await;
    let r = runner(&db, Provider::Openai, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Paused));
    assert_eq!(m.count(), 0, "nothing is sent when the worst case does not fit");
    let rec = db.lock().unwrap().session(sid).unwrap();
    assert!(rec.error.unwrap().contains("token budget"));
}

#[tokio::test]
async fn unknown_price_is_refused_without_acceptance() {
    let db = seed(3);
    let mut cfg = config(Provider::Openai, &[Task::Labels]);
    cfg.model = "unpriced-model".into();
    let sid = session(&db, &cfg);
    let m = mock(Arc::new(good_model(Provider::Openai))).await;
    let r = runner(&db, Provider::Openai, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Paused));
    assert_eq!(m.count(), 0);

    // Accepting unknown pricing lets it run; cost stays unknown, never zero.
    let mut cfg = cfg.clone();
    cfg.limits.allow_unknown_cost = true;
    let sid = session(&db, &cfg);
    let r = runner(&db, Provider::Openai, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    assert_eq!(r.cost, None);
    assert!(ledger_rows(&db).iter().all(|(_, c)| c.is_none()));
}

#[tokio::test]
async fn cancel_in_flight_charges_worst_case_and_stops() {
    let db = seed(3);
    let sid = session(&db, &config(Provider::Anthropic, &[Task::Labels, Task::Summary]));
    let slow = good_model(Provider::Anthropic);
    let m =
        mock(Arc::new(move |r: &Recorded, n: usize| MockReply { delay: Duration::from_secs(5), ..slow(r, n) })).await;
    let control = Control::default();
    let run = runner(&db, Provider::Anthropic, &m.base, control.clone());
    let started = std::time::Instant::now();
    let handle = tokio::spawn(async move { run.run(sid, YES).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    control.cancel();
    let r = handle.await.unwrap().unwrap();
    assert!(started.elapsed() < Duration::from_secs(3), "cancel does not wait for the provider");
    assert_eq!(r.final_status, Some(SessionStatus::Cancelled));
    let rows = ledger_rows(&db);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].0.ends_with(":cancelled_in_flight"));
    assert!(rows[0].1.unwrap() > 0, "worst case is charged, not zero");
    // Cancelled sessions cannot be resumed.
    let again = runner(&db, Provider::Anthropic, &m.base, Control::default()).run(sid, YES).await;
    assert!(matches!(again, Err(RunError::NotRunnable(SessionStatus::Cancelled))));
}

#[tokio::test]
async fn pause_between_calls_then_resume() {
    let db = seed(30);
    let sid = session(&db, &config(Provider::Gemini, &[Task::Labels]));
    let control = Control::default();
    // The user presses Pause while the first request is in flight.
    let pause_during_first = control.clone();
    let good = good_model(Provider::Gemini);
    let m = mock(Arc::new(move |r: &Recorded, n: usize| {
        if n == 0 {
            pause_during_first.pause();
        }
        good(r, n)
    }))
    .await;
    let r = runner(&db, Provider::Gemini, &m.base, control).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Paused));
    assert_eq!(m.count(), 1, "the in-flight request finishes; no new one starts");
    let r = runner(&db, Provider::Gemini, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    assert_eq!(m.count(), 2);
}

#[tokio::test]
async fn invalid_and_injected_output_is_discarded() {
    let db = seed(2);
    let sid = session(&db, &config(Provider::Openai, &[Task::Labels]));
    // The "model" tries to label articles outside the batch and returns junk.
    let m = mock(Arc::new(|_: &Recorded, n: usize| {
        let payload = if n == 0 {
            json!({"articles": [{"id": 999_999, "labels": ["Injected"]}]})
        } else {
            json!({"unexpected": true})
        };
        ok(envelope(Provider::Openai, &payload, 500, 50))
    }))
    .await;
    let r = runner(&db, Provider::Openai, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    assert_eq!(m.count(), 2, "one retry after unusable output");
    assert_eq!(r.calls, 2, "both calls are billed");
    let s = db.lock().unwrap();
    assert!(s.labels().unwrap().iter().all(|l| l.name != "Injected"));
    let n: i64 = s.conn().query_row("SELECT count(*) FROM article_labels", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn refusals_are_reported_not_fatal() {
    let db = seed(2);
    let sid = session(&db, &config(Provider::Anthropic, &[Task::Labels]));
    let m = mock(Arc::new(|_: &Recorded, _| {
        ok(json!({"content": [], "stop_reason": "refusal", "stop_details": {"category": "cyber"},
                  "usage": {"input_tokens": 100, "output_tokens": 0}}))
    }))
    .await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let r =
        runner(&db, Provider::Anthropic, &m.base, Control::default()).with_progress(tx).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    let mut notes = Vec::new();
    while let Ok(p) = rx.try_recv() {
        if let Progress::Note(n) = p {
            notes.push(n);
        }
    }
    assert!(notes.iter().any(|n| n.contains("declined")), "{notes:?}");
}

#[tokio::test]
async fn transient_errors_retry_and_auth_errors_fail_fast() {
    let db = seed(2);
    let sid = session(&db, &config(Provider::Anthropic, &[Task::Labels]));
    let good = good_model(Provider::Anthropic);
    let m = mock(Arc::new(move |r: &Recorded, n: usize| {
        if n < 2 {
            MockReply { status: 529, body: json!({"error": {"message": "overloaded"}}), delay: Duration::ZERO }
        } else {
            good(r, n)
        }
    }))
    .await;
    let r = runner(&db, Provider::Anthropic, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    assert_eq!(m.count(), 3);

    let sid = session(&db, &config(Provider::Anthropic, &[Task::Entities]));
    let m = mock(Arc::new(|_: &Recorded, _| MockReply {
        status: 401,
        body: json!({"error": {"message": "invalid x-api-key"}}),
        delay: Duration::ZERO,
    }))
    .await;
    let r = runner(&db, Provider::Anthropic, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Failed));
    assert_eq!(m.count(), 1);
    let rec = db.lock().unwrap().session(sid).unwrap();
    assert_eq!(rec.error.as_deref(), Some("The provider rejected the API key."));
    assert!(ledger_rows(&db).iter().all(|(op, _)| !op.contains("entities")), "failed auth is not charged");
}

#[tokio::test]
async fn preconditions_are_enforced() {
    let db = seed(2);
    let sid = session(&db, &config(Provider::Anthropic, &[Task::Labels]));
    let m = mock(Arc::new(good_model(Provider::Anthropic))).await;
    let no = Approval { consent_to_send: false };
    assert!(matches!(
        runner(&db, Provider::Anthropic, &m.base, Control::default()).run(sid, no).await,
        Err(RunError::NoConsent)
    ));
    assert!(matches!(
        runner(&db, Provider::Openai, &m.base, Control::default()).run(sid, YES).await,
        Err(RunError::ProviderMismatch { .. })
    ));
    // A draft session (no estimate, no approval step) cannot run.
    let draft = db
        .lock()
        .unwrap()
        .create_session(&serde_json::to_value(config(Provider::Anthropic, &[])).unwrap(), &[])
        .unwrap();
    assert!(matches!(
        runner(&db, Provider::Anthropic, &m.base, Control::default()).run(draft, YES).await,
        Err(RunError::NotRunnable(SessionStatus::Draft))
    ));
    assert_eq!(m.count(), 0);
}

#[tokio::test]
async fn provider_token_counting() {
    for provider in Provider::ALL {
        let m = mock(Arc::new(move |r: &Recorded, _| {
            ok(match provider {
                Provider::Gemini => {
                    assert!(r.path.ends_with(":countTokens"));
                    json!({"totalTokens": 321})
                }
                Provider::Anthropic => {
                    assert_eq!(r.path, "/v1/messages/count_tokens");
                    json!({"input_tokens": 321})
                }
                Provider::Openai => {
                    assert_eq!(r.path, "/v1/responses/input_tokens");
                    json!({"input_tokens": 321, "object": "response.input_tokens"})
                }
            })
        }))
        .await;
        let client =
            Client::new(Endpoint { provider, base_url: m.base.clone(), api_key: API_KEY.into() }, fast()).unwrap();
        let req = juttusihti_ai::Request {
            model: model_for(provider).into(),
            system: "s".into(),
            user: "u".into(),
            schema_name: "x".into(),
            schema: json!({"type": "object"}),
            max_output_tokens: 10,
        };
        assert_eq!(client.count_tokens(&req).await.unwrap(), 321, "{provider:?}");
    }
}

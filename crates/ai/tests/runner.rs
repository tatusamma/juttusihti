//! End-to-end runs against a scripted local server that imitates the
//! OpenAI, Anthropic and Gemini wire formats, and OpenAI's Sign in with
//! ChatGPT endpoints. No real provider is contacted and nothing here costs
//! money.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use juttusihti_ai::chatgpt::{AuthError, PendingSignIn, Registration, Servers, Session};
use juttusihti_ai::{AiError, Approval, Auth, Client, Control, Endpoint, Progress, RetryPolicy, RunError, Runner};
use juttusihti_core::analysis::{AnalysisConfig, ClickbaitCheck, Depth, Task};
use juttusihti_core::budget::Limits;
use juttusihti_core::estimate::estimate;
use juttusihti_core::pricing::{Billing, Catalog, Provider};
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
    /// The body as text (form-encoded OAuth requests are not JSON).
    raw: String,
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
                let rec = Recorded {
                    path,
                    headers,
                    body: serde_json::from_slice(&body).unwrap_or(Value::Null),
                    raw: String::from_utf8_lossy(&body).into_owned(),
                };
                let n = counter.fetch_add(1, Ordering::SeqCst);
                log.lock().unwrap().push(rec.clone());
                let reply = handler(&rec, n);
                tokio::time::sleep(reply.delay).await;
                // A string body is sent as is: a server-sent event stream.
                let (body, kind) = match &reply.body {
                    Value::String(sse) => (sse.clone(), "text/event-stream"),
                    other => (other.to_string(), "application/json"),
                };
                let head = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
        // The last input item: after the system message with an API key,
        // the only item on a ChatGPT plan (system text is in `instructions`).
        Provider::Openai => body["input"].as_array().and_then(|a| a.last()).and_then(|m| m["content"].as_str()),
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
                // A rewrite grounded in the article's own description.
                let text = a["text"].as_str().unwrap_or_default().trim_end_matches('.');
                json!({
                    "id": a["id"],
                    "labels": ["Politiikka"],
                    "entities": [{"name": first, "kind": "organization", "role": "subject", "evidence": first}],
                    "relevance": {"class": "relevant", "reason": "On topic."},
                    "headline": format!("{text} kertoo säästä"),
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
        billing: Default::default(),
        relevance_criteria: Some("työttömyysturva".into()),
        clickbait: juttusihti_core::analysis::ClickbaitCheck::off(),
        parallel_requests: 4,
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
    let client =
        Client::new(Endpoint { provider, base_url: base.into(), auth: Auth::ApiKey(API_KEY.into()) }, fast()).unwrap();
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
    // Batches are sent in parallel, so fail the second batch by its size, not arrival order.
    let failing = mock(Arc::new(move |r: &Recorded, n: usize| {
        if articles_in(&user_text(Provider::Anthropic, &r.body)).len() == 20 {
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
    // 100 articles: 5 extraction batches, more than one wave of parallel requests.
    let db = seed(98);
    let sid = session(&db, &config(Provider::Gemini, &[Task::Labels]));
    let control = Control::default();
    // The user presses Pause while the first wave is in flight.
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
    assert_eq!(m.count(), 4, "the in-flight wave finishes; no new request starts");
    let r = runner(&db, Provider::Gemini, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    assert_eq!(m.count(), 5, "resume sends only the unfinished batch");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn requests_run_in_parallel_waves() {
    let db = seed(98); // 100 articles → 5 extraction batches
    let sid = session(&db, &config(Provider::Openai, &[Task::Labels, Task::Headlines]));
    // Each answer of the first wave waits (up to 2 s) until 4 requests have
    // arrived, which only happens if they are sent at the same time.
    let arrived = Arc::new(AtomicUsize::new(0));
    let seen_together = Arc::new(AtomicUsize::new(0));
    let (count, together) = (arrived.clone(), seen_together.clone());
    let good = good_model(Provider::Openai);
    let m = mock(Arc::new(move |r: &Recorded, n: usize| {
        count.fetch_add(1, Ordering::SeqCst);
        if n < 4 {
            // block_in_place hands this worker's queued tasks to other workers.
            tokio::task::block_in_place(|| {
                let start = std::time::Instant::now();
                while count.load(Ordering::SeqCst) < 4 && start.elapsed() < Duration::from_secs(2) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
            together.fetch_max(count.load(Ordering::SeqCst), Ordering::SeqCst);
        }
        good(r, n)
    }))
    .await;
    let r = runner(&db, Provider::Openai, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    assert_eq!(m.count(), 5);
    assert_eq!(seen_together.load(Ordering::SeqCst), 4, "the first four batches are in flight together");
    let s = db.lock().unwrap();
    let rewritten: i64 = s
        .conn()
        .query_row("SELECT count(*) FROM article_headlines WHERE status = 'suggested'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rewritten, 98, "every 'Uutinen N' article gets a grounded rewrite");
    let h = s.headline(1).unwrap().unwrap();
    assert_eq!(h.headline, "Kuvaus numero 0 kertoo säästä");
}

#[tokio::test]
async fn low_reasoning_effort_is_dropped_if_the_model_rejects_it() {
    let db = seed(98); // 5 extraction batches
    let sid = session(&db, &config(Provider::Openai, &[Task::Labels]));
    let good = good_model(Provider::Openai);
    let m = mock(Arc::new(move |r: &Recorded, n| {
        if r.body.get("reasoning").is_some() {
            MockReply {
                status: 400,
                body: json!({"error": {"message": "Unsupported value: 'reasoning.effort' does not support 'low'."}}),
                delay: Duration::ZERO,
            }
        } else {
            good(r, n)
        }
    }))
    .await;
    let r = runner(&db, Provider::Openai, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    let reqs = m.requests();
    assert_eq!(reqs[0].body["reasoning"]["effort"], "low", "extraction asks for low effort");
    let rejected = reqs.iter().filter(|r| r.body.get("reasoning").is_some()).count();
    assert!(rejected <= 4, "at most the first parallel wave tries it: {rejected}");
    assert_eq!(reqs.len() - rejected, 5, "every batch succeeds without it");
    assert_eq!(r.calls, 5, "rejected requests are not billed");
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
            Client::new(Endpoint { provider, base_url: m.base.clone(), auth: Auth::ApiKey(API_KEY.into()) }, fast())
                .unwrap();
        let req = juttusihti_ai::Request {
            model: model_for(provider).into(),
            system: "s".into(),
            user: "u".into(),
            schema_name: "x".into(),
            schema: json!({"type": "object"}),
            max_output_tokens: 10,
            reasoning_effort: None,
        };
        assert_eq!(client.count_tokens(&req).await.unwrap(), 321, "{provider:?}");
    }
}

// ---- ChatGPT plan ---------------------------------------------------------------------

const PLAN_SCOPES: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";

fn servers(m: &Mock) -> Servers {
    Servers { auth_base: m.base.clone(), api_base: format!("{}/v1", m.base) }
}

type Saved = Arc<Mutex<Vec<Option<String>>>>;

fn persist(saved: &Saved) -> juttusihti_ai::chatgpt::Persist {
    let saved = saved.clone();
    Arc::new(move |t: Option<&str>| {
        saved.lock().unwrap().push(t.map(str::to_string));
        Ok(())
    })
}

fn form_of(raw: &str) -> std::collections::HashMap<String, String> {
    url::form_urlencoded::parse(raw.as_bytes()).into_owned().collect()
}

fn sse_completed(response: &Value) -> Value {
    Value::String(format!(
        "event: response.created\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
        json!({"type": "response.created", "response": {"status": "in_progress"}}),
        json!({"type": "response.completed", "response": response})
    ))
}

/// Token endpoint that rotates refresh tokens r1 → r2 and rejects reuse.
fn refresh_reply(r: &Recorded) -> MockReply {
    let f = form_of(&r.raw);
    match f.get("refresh_token").map(String::as_str) {
        Some("r1") => ok(json!({"access_token": "plan-access", "refresh_token": "r2", "expires_in": 3600,
                                "scope": PLAN_SCOPES, "token_type": "Bearer"})),
        _ => MockReply { status: 400, body: json!({"error": "invalid_grant"}), delay: Duration::ZERO },
    }
}

#[tokio::test]
async fn chatgpt_plan_run_streams_and_has_no_api_cost() {
    let db = seed(3);
    let mut cfg = config(Provider::Openai, &[Task::Labels, Task::Entities, Task::Related, Task::Summary]);
    cfg.billing = Billing::ChatGptPlan;
    cfg.model = "gpt-plan-only".into(); // not in the price catalog: still a known zero
    let sid = session(&db, &cfg);
    let good = good_model(Provider::Openai);
    let m = mock(Arc::new(move |r: &Recorded, n| {
        if r.path == "/api/accounts/oauth/token" {
            return refresh_reply(r);
        }
        ok(sse_completed(&good(r, n).body))
    }))
    .await;
    let saved: Saved = Default::default();
    let session = Session::resume(servers(&m), "oaiapp_test".into(), "r1".into(), persist(&saved)).unwrap();
    let client = Client::new(Endpoint::chatgpt(Arc::new(session)), fast()).unwrap();
    let summary = Runner::new(db.clone(), client, Catalog::builtin(), Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(summary.final_status, Some(SessionStatus::Completed));
    assert_eq!(summary.cost, Some(0), "charged to the plan, not unknown");
    assert!(summary.usage.total() > 0, "tokens are still counted");
    assert_eq!(*saved.lock().unwrap(), vec![Some("r2".to_string())], "rotated refresh token is persisted");

    let reqs = m.requests();
    let token: Vec<_> = reqs.iter().filter(|r| r.path == "/api/accounts/oauth/token").collect();
    assert_eq!(token.len(), 1, "one refresh serves the whole run");
    let f = form_of(&token[0].raw);
    assert_eq!(f["grant_type"], "refresh_token");
    assert_eq!(f["client_id"], "oaiapp_test");
    assert_eq!(f["resource"], "https://api.openai.com/v1");
    assert!(!f.contains_key("scope"), "omitting scope keeps the grant");
    let api: Vec<_> = reqs.iter().filter(|r| r.path == "/v1/responses").collect();
    assert_eq!(api.len(), 3, "extraction, related, summary");
    for r in api {
        assert!(r.headers.to_ascii_lowercase().contains("authorization: bearer plan-access"));
        assert_eq!(r.body["stream"], true);
        assert_eq!(r.body["store"], false);
        assert!(r.body.get("max_output_tokens").is_none(), "rejected on a plan");
        assert!(r.body["instructions"].as_str().is_some_and(|s| !s.is_empty()));
        assert_eq!(r.body["input"].as_array().unwrap().len(), 1, "no system items in input");
    }
    assert!(ledger_rows(&db).iter().all(|(_, cost)| *cost == Some(0)));
}

#[tokio::test]
async fn chatgpt_plan_limit_pauses_without_retrying() {
    let db = seed(2);
    let mut cfg = config(Provider::Openai, &[Task::Labels]);
    cfg.billing = Billing::ChatGptPlan;
    let sid = session(&db, &cfg);
    let m = mock(Arc::new(|r: &Recorded, _| {
        if r.path == "/api/accounts/oauth/token" {
            return refresh_reply(r);
        }
        MockReply {
            status: 429,
            body: json!({"error": {"code": "subscription_sharing_usage_limit_exceeded", "message": "Weekly app limit"}}),
            delay: Duration::ZERO,
        }
    }))
    .await;
    let session =
        Session::resume(servers(&m), "oaiapp_test".into(), "r1".into(), persist(&Default::default())).unwrap();
    let client = Client::new(Endpoint::chatgpt(Arc::new(session)), fast()).unwrap();
    let summary = Runner::new(db.clone(), client, Catalog::builtin(), Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(summary.final_status, Some(SessionStatus::Paused));
    let rec = db.lock().unwrap().session(sid).unwrap();
    assert!(rec.error.unwrap().contains("usage limit"));
    assert_eq!(m.requests().iter().filter(|r| r.path == "/v1/responses").count(), 1, "no retries");
}

#[tokio::test]
async fn billing_must_match_the_approved_session() {
    let db = seed(1);
    let mut cfg = config(Provider::Openai, &[Task::Labels]);
    cfg.billing = Billing::ChatGptPlan;
    let sid = session(&db, &cfg);
    let err = runner(&db, Provider::Openai, "http://127.0.0.1:9", Control::default()).run(sid, YES).await.unwrap_err();
    assert!(matches!(err, RunError::BillingMismatch { .. }), "{err:?}");
}

#[tokio::test]
async fn chatgpt_refresh_failure_signs_out_and_counting_is_unsupported() {
    let m = mock(Arc::new(|r: &Recorded, _| refresh_reply(r))).await;
    let saved: Saved = Default::default();
    let session = Arc::new(Session::resume(servers(&m), "oaiapp_test".into(), "dead".into(), persist(&saved)).unwrap());
    assert_eq!(session.access_token().await, Err(AuthError::SignedOut));
    assert_eq!(*saved.lock().unwrap(), vec![None], "unusable token is cleared");

    let client = Client::new(Endpoint::chatgpt(session), fast()).unwrap();
    let req = juttusihti_ai::Request {
        model: "m".into(),
        system: "s".into(),
        user: "u".into(),
        schema_name: "x".into(),
        schema: json!({"type": "object"}),
        max_output_tokens: 10,
        reasoning_effort: None,
    };
    assert!(matches!(client.count_tokens(&req).await, Err(AiError::Unsupported(_))));
    assert!(matches!(client.complete(&req).await, Err(AiError::Auth(AuthError::SignedOut))));
}

/// What the fake authorization server issues next.
#[derive(Default)]
struct AuthScript {
    challenge: String,
    nonce: String,
    subject: String,
    scope: String,
}

fn id_token(claims: Value) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    format!("{}.{}.sig", b64.encode(r#"{"alg":"RS256"}"#), b64.encode(claims.to_string()))
}

#[tokio::test]
async fn chatgpt_sign_in_registers_then_returns() {
    use sha2::Digest;
    let script = Arc::new(Mutex::new(AuthScript::default()));
    let base = Arc::new(Mutex::new(String::new()));
    let (sc, b) = (script.clone(), base.clone());
    let m = mock(Arc::new(move |r: &Recorded, _| {
        let f = form_of(&r.raw);
        let s = sc.lock().unwrap();
        use base64::Engine;
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(sha2::Sha256::digest(f.get("code_verifier").cloned().unwrap_or_default().as_bytes()));
        if f.get("grant_type").map(String::as_str) != Some("authorization_code")
            || f.get("code").map(String::as_str) != Some("code-1")
            || challenge != s.challenge
        {
            return MockReply { status: 400, body: json!({"error": "invalid_grant"}), delay: Duration::ZERO };
        }
        let claims = json!({"iss": *b.lock().unwrap(), "aud": f["client_id"], "nonce": s.nonce,
                            "exp": chrono::Utc::now().timestamp() + 600, "sub": s.subject, "email": "user@example.com"});
        ok(json!({"access_token": "a1", "refresh_token": "r1", "id_token": id_token(claims),
                  "expires_in": 3600, "scope": s.scope}))
    }))
    .await;
    *base.lock().unwrap() = m.base.clone();

    // Drives one browser round trip; returns the authorization URL's query.
    async fn sign_in(
        m: &Mock,
        script: &Arc<Mutex<AuthScript>>,
        reg: Registration,
        subject: &str,
        scope: &str,
    ) -> (std::collections::HashMap<String, String>, Result<juttusihti_ai::chatgpt::SignedIn, AuthError>) {
        let pending = PendingSignIn::start(servers(m), reg).await.unwrap();
        let url = url::Url::parse(pending.url()).unwrap();
        assert!(url.as_str().starts_with(&format!("{}/api/accounts/authorize?", m.base)));
        let q: std::collections::HashMap<String, String> = url.query_pairs().into_owned().collect();
        *script.lock().unwrap() = AuthScript {
            challenge: q["code_challenge"].clone(),
            nonce: q["nonce"].clone(),
            subject: subject.into(),
            scope: scope.into(),
        };
        let redirect = q["redirect_uri"].clone();
        let finish = tokio::spawn(pending.finish());
        let http = reqwest::Client::new();
        // A stray local request with the wrong state is ignored.
        let stray = http.get(format!("{redirect}?code=evil&state=wrong")).send().await.unwrap();
        assert_eq!(stray.status(), 400);
        let back = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("code", "code-1")
            .append_pair("state", &q["state"])
            .append_pair("scope", scope)
            .append_pair("client_id", q["client_id"].replace("dynamic_agent_client", "oaiapp_new").as_str())
            .finish();
        let page = http.get(format!("{redirect}?{back}")).send().await.unwrap();
        assert_eq!(page.status(), 200);
        (q, finish.await.unwrap())
    }

    // First sign-in: dynamic registration.
    let (q, result) = sign_in(&m, &script, Registration::new(), "user-1", PLAN_SCOPES).await;
    assert_eq!(q["client_id"], "dynamic_agent_client");
    assert_eq!(q["agent_name_hint"], "Juttusihti");
    assert_eq!(q["scope"], PLAN_SCOPES);
    assert_eq!(q["resource"], "https://api.openai.com/v1");
    assert_eq!(q["code_challenge_method"], "S256");
    assert!(q["redirect_uri"].starts_with("http://127.0.0.1:") && q["redirect_uri"].ends_with("/auth/callback"));
    assert!(q["ext_agent_host_id"].starts_with("urn:uuid:"));
    let signed = result.unwrap();
    assert_eq!(signed.registration.client_id.as_deref(), Some("oaiapp_new"));
    assert_eq!(signed.registration.subject.as_deref(), Some("user-1"));
    assert_eq!(signed.registration.email.as_deref(), Some("user@example.com"));
    assert_eq!(signed.refresh_token, "r1");
    let exchange = form_of(&m.requests().last().unwrap().raw);
    assert_eq!(exchange["client_id"], "oaiapp_new");
    assert_eq!(exchange["redirect_uri"], q["redirect_uri"]);
    assert!(!exchange.contains_key("client_secret"));

    // Returning sign-in reuses the issued client id and hints the account.
    let reg = signed.registration.clone();
    let (q, result) = sign_in(&m, &script, reg.clone(), "user-1", PLAN_SCOPES).await;
    assert_eq!(q["client_id"], "oaiapp_new");
    assert_eq!(q["login_hint"], "user@example.com");
    assert!(!q.contains_key("agent_name_hint"));
    assert_eq!(q["ext_agent_host_id"], reg.host_id, "host id is stable");
    assert!(result.is_ok());

    // Another account, or no plan scope, is refused.
    let (_, result) = sign_in(&m, &script, reg.clone(), "user-2", PLAN_SCOPES).await;
    assert_eq!(result.unwrap_err(), AuthError::DifferentAccount);
    let (_, result) = sign_in(&m, &script, reg, "user-1", "openid profile email offline_access").await;
    assert_eq!(result.unwrap_err(), AuthError::NotEligible);
}

// ---- clickbait check -----------------------------------------------------------------

/// Test headlines with how likely the fake classifier rates each one clickbait.
const HEADLINES: &[(&str, Option<&str>, f64)] = &[
    ("Et usko, mitä eduskunnassa tapahtui", Some("Eduskunta hyväksyi budjetin äänin 101–90."), 0.95),
    ("Eduskunta hyväksyi ensi vuoden budjetin", Some("Äänestys päättyi lukemiin 101–90."), 0.05),
    // Dramatic but factual.
    ("Maanjäristys tuhosi kaupungin keskustan", Some("Järistyksen voimakkuus oli 7,1."), 0.20),
    // Ambiguous, exactly at the threshold.
    ("Onko tämä talven kylmin päivä?", Some("Lämpötila laski Sallassa -38 asteeseen."), 0.70),
    // A quotation, just below it.
    ("Pääministeri: \"Emme nosta veroja\"", Some("Pääministeri lupasi pitää verot ennallaan."), 0.69),
    // No summary: no rewrite could be grounded, so it is never sent.
    ("Tämä yksi asia muutti kaiken", None, 0.99),
];

/// The headlines above, plus a copy of the first under another address
/// (same content, so it is rated once).
fn seed_headlines() -> Arc<Mutex<Store>> {
    let mut s = Store::open_in_memory().unwrap();
    let f =
        s.add_feed(&NewFeed { url: "https://f.example/rss".into(), title: "F".into(), ..Default::default() }).unwrap();
    let mut entries: Vec<NewArticle> = HEADLINES
        .iter()
        .enumerate()
        .map(|(i, (title, summary, _))| NewArticle {
            guid: format!("h{i}"),
            url: Some(format!("https://f.example/h{i}")),
            title: (*title).into(),
            summary: summary.map(str::to_string),
            content: None,
            author: None,
            published_at: Some(OCT1 + i as i64 * 600),
        })
        .collect();
    entries.push(NewArticle {
        guid: "copy".into(),
        url: Some("https://other.example/copy".into()),
        published_at: Some(OCT1 + DAY),
        ..entries[0].clone()
    });
    s.ingest(f, &entries).unwrap();
    Arc::new(Mutex::new(s))
}

fn check_config(tasks: &[Task]) -> AnalysisConfig {
    let mut c = config(Provider::Openai, tasks);
    c.clickbait = ClickbaitCheck { batch_size: 2, ..Default::default() };
    c.relevance_criteria = None;
    c
}

fn is_decision(r: &Recorded) -> bool {
    r.path.ends_with("/v1/decisions")
}

/// Rates titles from [`HEADLINES`] (answers in reverse order, so mapping
/// must go by name) and rewrites headlines from the article's own text.
fn classifier_and_rewriter(r: &Recorded, _: usize) -> MockReply {
    if is_decision(r) {
        let titles: std::collections::HashMap<i64, String> = r.body["input"][1]["content"][0]["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| (a["id"].as_i64().unwrap(), a["title"].as_str().unwrap().to_string()))
            .collect();
        let mut answers: Vec<Value> = r.body["questions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|q| {
                let name = q["name"].as_str().unwrap();
                let id: i64 = name.strip_prefix("article-").unwrap().parse().unwrap();
                let p = HEADLINES.iter().find(|h| h.0 == titles[&id]).map_or(0.0, |h| h.2);
                json!({"type": "predicate", "name": name, "probability": p})
            })
            .collect();
        answers.reverse();
        return ok(json!({"answers": answers, "model": "gpt-6-luna",
            "usage": {"input_tokens": 400, "output_tokens": 8, "total_tokens": 408}}));
    }
    let items = articles_in(&user_text(Provider::Openai, &r.body));
    let wants_headline = asks_headline(r);
    let payload = json!({"articles": items.iter().map(|a| json!({
        "id": a["id"],
        "labels": ["Politiikka"],
        "headline": if wants_headline { a["text"].as_str().unwrap_or_default().trim_end_matches('.') } else { "" },
    })).collect::<Vec<_>>()});
    ok(envelope(Provider::Openai, &payload, 1_000, 200))
}

fn asks_headline(r: &Recorded) -> bool {
    r.body["text"]["format"]["schema"]["properties"]["articles"]["items"]["properties"].get("headline").is_some()
}

fn titles_sent(r: &Recorded) -> Vec<String> {
    let mut titles: Vec<String> = articles_in(&user_text(Provider::Openai, &r.body))
        .iter()
        .map(|a| a["title"].as_str().unwrap_or_default().to_string())
        .collect();
    titles.sort();
    titles
}

fn rewritten(db: &Arc<Mutex<Store>>) -> Vec<String> {
    let s = db.lock().unwrap();
    let mut stmt = s.conn().prepare("SELECT headline FROM article_headlines ORDER BY headline").unwrap();
    stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
}

#[tokio::test]
async fn clickbait_check_sends_only_likely_clickbait_to_the_rewriting_model() {
    let db = seed_headlines();
    let sid = session(&db, &check_config(&[Task::Headlines]));
    let m = mock(Arc::new(classifier_and_rewriter)).await;
    let r = runner(&db, Provider::Openai, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));

    let (checks, rewrites): (Vec<Recorded>, Vec<Recorded>) = m.requests().into_iter().partition(is_decision);
    // Five distinct contents with text, two questions per request.
    let questions: Vec<usize> = checks.iter().map(|c| c.body["questions"].as_array().unwrap().len()).collect();
    assert_eq!(questions.iter().sum::<usize>(), 5, "the copy is rated once, the one without text never");
    assert!(questions.iter().all(|n| *n <= 2), "{questions:?}");
    assert!(checks.iter().all(|c| c.headers.contains(API_KEY) && c.body["model"] == "gpt-6-luna"));

    assert_eq!(rewrites.len(), 1, "one rewriting request for all likely clickbait");
    assert_eq!(
        titles_sent(&rewrites[0]),
        [
            "Et usko, mitä eduskunnassa tapahtui",
            "Et usko, mitä eduskunnassa tapahtui",
            "Onko tämä talven kylmin päivä?"
        ],
        "factual, dramatic, quoted and summary-less headlines never reach the rewriting model"
    );
    assert_eq!(
        rewritten(&db),
        [
            "Eduskunta hyväksyi budjetin äänin 101–90",
            "Eduskunta hyväksyi budjetin äänin 101–90",
            "Lämpötila laski Sallassa -38 asteeseen"
        ]
    );
    let h = &r.headlines;
    assert_eq!((h.considered, h.classified, h.cached), (6, 5, 0));
    assert_eq!((h.clickbait, h.rewrites_avoided, h.fallbacks), (3, 3, 0));
    assert_eq!((h.check_requests, h.check_errors), (3, 0));
    let ledger = ledger_rows(&db);
    let checks: Vec<&(String, Option<i64>)> = ledger.iter().filter(|(op, _)| op == "clickbait_check").collect();
    assert_eq!(checks.len(), 3);
    // 400 uncached input tokens at $0.10 / M; output tokens are not charged.
    assert!(checks.iter().all(|(_, c)| *c == Some(40)), "{ledger:?}");
}

#[tokio::test]
async fn clickbait_results_are_cached_and_a_new_threshold_reuses_them() {
    let db = seed_headlines();
    let m = mock(Arc::new(classifier_and_rewriter)).await;
    let cfg = check_config(&[Task::Headlines]);
    runner(&db, Provider::Openai, &m.base, Control::default()).run(session(&db, &cfg), YES).await.unwrap();
    let first = m.count();

    // The same run again needs nothing at all.
    let again = runner(&db, Provider::Openai, &m.base, Control::default()).run(session(&db, &cfg), YES).await.unwrap();
    assert_eq!(again.final_status, Some(SessionStatus::Completed));
    assert_eq!(m.count(), first, "no request repeats paid work");

    // A lower threshold rewrites more, from the cached probabilities.
    let mut strict = cfg.clone();
    strict.clickbait.threshold_percent = 15;
    let r = runner(&db, Provider::Openai, &m.base, Control::default()).run(session(&db, &strict), YES).await.unwrap();
    let new: Vec<Recorded> = m.requests().split_off(first);
    assert!(new.iter().all(|r| !is_decision(r)), "probabilities are reused");
    assert_eq!(new.len(), 1);
    assert_eq!(
        titles_sent(&new[0]),
        ["Maanjäristys tuhosi kaupungin keskustan", "Pääministeri: \"Emme nosta veroja\""]
    );
    assert_eq!((r.headlines.cached, r.headlines.classified), (2, 0));
}

#[tokio::test]
async fn non_clickbait_articles_skip_the_headline_field_when_combined_with_labels() {
    let db = seed_headlines();
    let sid = session(&db, &check_config(&[Task::Labels, Task::Headlines]));
    let m = mock(Arc::new(classifier_and_rewriter)).await;
    let r = runner(&db, Provider::Openai, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    let rewrites: Vec<Recorded> = m.requests().into_iter().filter(|r| !is_decision(r)).collect();
    assert_eq!(rewrites.len(), 2, "one batch with headlines, one without");
    let (with, without): (Vec<&Recorded>, Vec<&Recorded>) = rewrites.iter().partition(|r| asks_headline(r));
    assert_eq!(titles_sent(with[0]).len(), 3);
    assert_eq!(titles_sent(without[0]).len(), 4, "labels still cover every article");
    assert_eq!(rewritten(&db).len(), 3);
    let s = db.lock().unwrap();
    let labeled: i64 =
        s.conn().query_row("SELECT count(DISTINCT article_id) FROM article_labels", [], |r| r.get(0)).unwrap();
    assert_eq!(labeled, 7);
}

#[tokio::test]
async fn a_failing_clickbait_check_falls_back_to_the_rewriting_model() {
    let db = seed_headlines();
    let mut cfg = check_config(&[Task::Headlines]);
    cfg.clickbait.batch_size = 50;
    let sid = session(&db, &cfg);
    let m = mock(Arc::new(|r: &Recorded, n| {
        if is_decision(r) {
            MockReply { status: 503, body: json!({"error": {"message": "overloaded"}}), delay: Duration::ZERO }
        } else {
            classifier_and_rewriter(r, n)
        }
    }))
    .await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let r = runner(&db, Provider::Openai, &m.base, Control::default()).with_progress(tx).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed), "the run is not interrupted");
    let checks = m.requests().into_iter().filter(is_decision).count();
    assert_eq!(checks, 3, "transient errors are retried with backoff, then the check gives up");
    assert_eq!((r.headlines.fallbacks, r.headlines.check_errors, r.headlines.rewrites_avoided), (6, 1, 0));
    assert_eq!(rewritten(&db).len(), 6, "every headline with text went to the rewriting model, as without the check");
    assert!(ledger_rows(&db).iter().all(|(op, _)| !op.starts_with("clickbait_check")), "errors are not charged");
    let mut notes = Vec::new();
    while let Ok(p) = rx.try_recv() {
        if let Progress::Note(n) = p {
            notes.push(n);
        }
    }
    assert!(notes.iter().any(|n| n.contains("Clickbait check failed")), "{notes:?}");
}

#[tokio::test]
async fn a_clickbait_check_timeout_is_counted_at_its_maximum_and_falls_back() {
    let db = seed_headlines();
    let mut cfg = check_config(&[Task::Headlines]);
    (cfg.clickbait.batch_size, cfg.clickbait.timeout_secs) = (50, 1);
    let sid = session(&db, &cfg);
    let m = mock(Arc::new(|r: &Recorded, n| {
        let mut reply = classifier_and_rewriter(r, n);
        if is_decision(r) {
            reply.delay = Duration::from_secs(3);
        }
        reply
    }))
    .await;
    let r = runner(&db, Provider::Openai, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    assert_eq!(r.headlines.fallbacks, 6);
    assert_eq!(m.requests().into_iter().filter(is_decision).count(), 1, "an unknown outcome is not retried");
    assert!(ledger_rows(&db).iter().any(|(op, _)| op == "clickbait_check:unknown_outcome"));
}

#[tokio::test]
async fn clickbait_check_is_skipped_on_other_providers() {
    let db = seed_headlines();
    let mut cfg = check_config(&[Task::Headlines]);
    (cfg.provider, cfg.model) = (Provider::Anthropic, model_for(Provider::Anthropic).into());
    let sid = session(&db, &cfg);
    let m = mock(Arc::new(good_model(Provider::Anthropic))).await;
    let r = runner(&db, Provider::Anthropic, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    assert!(m.requests().iter().all(|r| !is_decision(r)));
    assert_eq!((r.headlines.considered, r.headlines.check_requests), (0, 0));
}

// ---- an empty OpenAI API account -----------------------------------------------------

fn no_credit() -> MockReply {
    MockReply {
        status: 429,
        body: json!({"error": {"message": "You have no credits remaining. Add credits to continue using the API.",
            "type": "insufficient_quota", "code": "insufficient_quota"}}),
        delay: Duration::ZERO,
    }
}

#[tokio::test]
async fn no_credit_pauses_without_retrying_and_resumes_after_a_top_up() {
    let db = seed(3);
    let sid = session(&db, &config(Provider::Openai, &[Task::Labels]));
    let topped_up = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let paid = topped_up.clone();
    let good = good_model(Provider::Openai);
    let m =
        mock(Arc::new(move |r: &Recorded, n| if paid.load(Ordering::SeqCst) { good(r, n) } else { no_credit() })).await;
    let r = runner(&db, Provider::Openai, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Paused), "resumable, not failed");
    assert_eq!(m.count(), 1, "an empty account is not retried");
    assert!(r.stop_reason.as_deref().unwrap().contains("no credit left"), "{:?}", r.stop_reason);
    assert!(ledger_rows(&db).is_empty(), "a refused request is not charged");

    topped_up.store(true, Ordering::SeqCst);
    let r = runner(&db, Provider::Openai, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    assert_eq!(r.stop_reason, None);
}

#[tokio::test]
async fn no_credit_on_the_clickbait_check_pauses_before_any_rewrite() {
    let db = seed_headlines();
    let mut cfg = check_config(&[Task::Headlines]);
    cfg.clickbait.batch_size = 50;
    let sid = session(&db, &cfg);
    let m = mock(Arc::new(|_: &Recorded, _| no_credit())).await;
    let r = runner(&db, Provider::Openai, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Paused));
    assert_eq!(m.count(), 1, "one check request; no rewrite is sent to an empty account");
    assert!(is_decision(&m.requests()[0]));
    assert!(r.stop_reason.as_deref().unwrap().contains("platform.openai.com"), "{:?}", r.stop_reason);
}

// ---- the clickbait check on API credit, everything else on the plan -------------------

fn split_config() -> AnalysisConfig {
    let mut c = check_config(&[Task::Headlines]);
    c.billing = Billing::ChatGptPlan;
    c.clickbait.on_api_credit = true;
    c
}

/// A runner paying with the plan, and with the API key for the check.
fn split_runner(db: &Arc<Mutex<Store>>, m: &Mock, saved: &Saved) -> Runner {
    let session = Session::resume(servers(m), "oaiapp_test".into(), "r1".into(), persist(saved)).unwrap();
    let plan = Client::new(Endpoint::chatgpt(Arc::new(session)), fast()).unwrap();
    let key = Endpoint { provider: Provider::Openai, base_url: m.base.clone(), auth: Auth::ApiKey(API_KEY.into()) };
    let check = Client::new(key, fast()).unwrap();
    Runner::new(db.clone(), plan, Catalog::builtin(), Control::default()).with_check_client(check)
}

/// The classifier answers with JSON, the rewriter streams on the plan.
fn split_reply(r: &Recorded, n: usize) -> MockReply {
    if r.path == "/api/accounts/oauth/token" {
        return refresh_reply(r);
    }
    let reply = classifier_and_rewriter(r, n);
    if is_decision(r) { reply } else { ok(sse_completed(&reply.body)) }
}

fn has_bearer(r: &Recorded, token: &str) -> bool {
    r.headers.to_ascii_lowercase().contains(&format!("authorization: bearer {token}"))
}

#[tokio::test]
async fn a_split_run_sends_the_check_on_the_key_and_the_rewrites_on_the_plan() {
    let db = seed_headlines();
    let sid = session(&db, &split_config());
    let m = mock(Arc::new(split_reply)).await;
    let r = split_runner(&db, &m, &Default::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    assert_eq!((r.headlines.clickbait, r.headlines.rewrites_avoided), (3, 3));

    let reqs = m.requests();
    let checks: Vec<_> = reqs.iter().filter(|r| is_decision(r)).collect();
    let rewrites: Vec<_> = reqs.iter().filter(|r| r.path == "/v1/responses").collect();
    assert_eq!(checks.len(), 3);
    assert_eq!(rewrites.len(), 1, "one rewriting request for all likely clickbait");
    let key = API_KEY.to_ascii_lowercase();
    assert!(checks.iter().all(|r| has_bearer(r, &key) && !has_bearer(r, "plan-access")), "check on the key");
    assert!(rewrites.iter().all(|r| has_bearer(r, "plan-access") && !has_bearer(r, &key)), "rewrites on the plan");

    // The check is real money, the rewrites a known zero; the summary adds them up.
    let ledger = ledger_rows(&db);
    let cost_of = |op: &str| -> Vec<Option<i64>> {
        ledger.iter().filter(|(o, _)| (o == "clickbait_check") == (op == "check")).map(|(_, c)| *c).collect()
    };
    assert_eq!(cost_of("check"), [Some(40); 3], "{ledger:?}");
    assert!(cost_of("rest").iter().all(|c| *c == Some(0)), "{ledger:?}");
    assert_eq!(r.cost, Some(120));
    assert_eq!(rewritten(&db).len(), 3);
}

#[tokio::test]
async fn a_split_run_stays_within_the_cost_limit() {
    let db = seed_headlines();
    let mut cfg = split_config();
    cfg.limits.max_cost = Some(1);
    let sid = session(&db, &cfg);
    let m = mock(Arc::new(split_reply)).await;
    let r = split_runner(&db, &m, &Default::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed), "the check is skipped, the plan work goes on");
    assert!(m.requests().iter().all(|r| !is_decision(r)), "the limit stops the check before it is sent");
    assert_eq!(r.cost, Some(0));
}

#[tokio::test]
async fn no_credit_on_a_split_run_continues_on_the_plan_without_the_check() {
    let db = seed_headlines();
    let mut cfg = split_config();
    cfg.clickbait.batch_size = 50;
    let sid = session(&db, &cfg);
    let m = mock(Arc::new(|r: &Recorded, n| if is_decision(r) { no_credit() } else { split_reply(r, n) })).await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let r = split_runner(&db, &m, &Default::default()).with_progress(tx).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed), "the plan still pays for the rewrites");
    assert_eq!(r.stop_reason, None);
    assert_eq!(m.requests().iter().filter(|r| is_decision(r)).count(), 1, "an empty account is not retried");
    assert_eq!((r.headlines.classified, r.headlines.fallbacks, r.headlines.check_errors), (0, 6, 1));
    assert_eq!(rewritten(&db).len(), 6, "every headline goes to the rewriting model");
    let mut notes = Vec::new();
    while let Ok(p) = rx.try_recv() {
        if let Progress::Note(n) = p {
            notes.push(n);
        }
    }
    assert!(notes.iter().any(|n| n.contains("no credit left") && !n.contains("  ")), "{notes:?}");
    assert_eq!(r.cost, Some(0), "a refused check is not charged");
}

#[tokio::test]
async fn a_split_run_needs_the_key_for_its_check() {
    let db = seed_headlines();
    let sid = session(&db, &split_config());
    let m = mock(Arc::new(split_reply)).await;
    let saved: Saved = Default::default();
    let plan = |m: &Mock| {
        let session = Session::resume(servers(m), "oaiapp_test".into(), "r1".into(), persist(&saved)).unwrap();
        Client::new(Endpoint::chatgpt(Arc::new(session)), fast()).unwrap()
    };
    let without = Runner::new(db.clone(), plan(&m), Catalog::builtin(), Control::default());
    assert!(matches!(without.run(sid, YES).await.unwrap_err(), RunError::NoCheckClient));
    // The plan's own token does not count as the key.
    let wrong = Runner::new(db.clone(), plan(&m), Catalog::builtin(), Control::default()).with_check_client(plan(&m));
    assert!(matches!(wrong.run(sid, YES).await.unwrap_err(), RunError::NoCheckClient));
    assert_eq!(m.count(), 0, "nothing is sent");
    // An API key can't stand in for the plan either.
    let key = runner(&db, Provider::Openai, &m.base, Control::default());
    assert!(matches!(key.run(sid, YES).await.unwrap_err(), RunError::BillingMismatch { .. }));
}

#[tokio::test]
async fn a_paused_split_run_resumes_and_repeats_nothing() {
    let db = seed_headlines();
    let sid = session(&db, &split_config());
    let plan_back = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let back = plan_back.clone();
    let m = mock(Arc::new(move |r: &Recorded, n| {
        // The plan's limit on the rewrite pauses the run after the check.
        if r.path == "/v1/responses" && !back.load(Ordering::SeqCst) {
            return MockReply {
                status: 429,
                body: json!({"error": {"code": "subscription_sharing_usage_limit_exceeded", "message": "limit"}}),
                delay: Duration::ZERO,
            };
        }
        split_reply(r, n)
    }))
    .await;
    let saved: Saved = Default::default();
    let r = split_runner(&db, &m, &saved).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Paused));
    let checks = |m: &Mock| m.requests().iter().filter(|r| is_decision(r)).count();
    let first = checks(&m);
    assert_eq!(first, 3);

    plan_back.store(true, Ordering::SeqCst);
    let r = split_runner(&db, &m, &saved).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    assert_eq!(checks(&m), first, "ratings are remembered");
    assert_eq!(rewritten(&db).len(), 3);
}

#[tokio::test]
async fn a_rate_limit_is_still_retried() {
    let db = seed(3);
    let sid = session(&db, &config(Provider::Openai, &[Task::Labels]));
    let good = good_model(Provider::Openai);
    let m = mock(Arc::new(move |r: &Recorded, n| {
        if n == 0 {
            MockReply {
                status: 429,
                body: json!({"error": {"message": "Rate limit reached", "type": "requests", "code": "rate_limit_exceeded"}}),
                delay: Duration::ZERO,
            }
        } else {
            good(r, n)
        }
    }))
    .await;
    let r = runner(&db, Provider::Openai, &m.base, Control::default()).run(sid, YES).await.unwrap();
    assert_eq!(r.final_status, Some(SessionStatus::Completed));
    assert_eq!(m.count(), 2);
}

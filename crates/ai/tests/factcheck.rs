//! End-to-end fact checks against local servers: one imitates the OpenAI
//! Responses API, the other Google Fact Check Tools and Wikipedia search.
//! No real service is contacted and nothing here costs money.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use juttusihti_ai::factcheck::{ArticleForCheck, FactCheckError, FactCheckJob, FactCheckProgress, run_fact_check};
use juttusihti_ai::sources::Sources;
use juttusihti_ai::{Auth, Client, Control, Endpoint, RetryPolicy};
use juttusihti_core::factcheck::{FACT_CHECK_PROMPT_VERSION, FactCheck, SourceKind, Verdict};
use juttusihti_core::pricing::{Billing, Catalog, Micros, Provider};
use juttusihti_core::store::Store;
use juttusihti_core::{ArticleId, NewArticle, NewFeed};
use serde_json::{Value, json};
use tokio::sync::mpsc;

mod common;
use common::{Mock, MockReply, Recorded, mock};

const MODEL: &str = "gpt-test";
const ARTICLE_URL: &str = "https://news.example/a";
const CLAIM_1: &str = "Kaupungissa on 5000 asukasta enemmän kuin viime vuonna.";
const CLAIM_2: &str = "Uusi silta maksoi 40 miljoonaa euroa.";
const CLAIM_3: &str = "Kunnanvaltuusto hyväksyi budjetin yksimielisesti.";
const FAILED: &str = "The check could not be completed for this claim.";
const WEB_SOURCE: &str = "https://other.example/report";
const UNCONFIRMED: &str = "No independent source could be confirmed for this verdict.";

/// $1 per million input tokens, $2 per million output tokens, $100 per 1000 searches.
fn catalog() -> Catalog {
    Catalog::from_json(&format!(
        r#"{{"entries":[{{"provider":"openai","model":"{MODEL}","input":1,"output":2,"web_search_per_1k":100,"source":"test"}}]}}"#
    ))
    .unwrap()
}

/// Token prices known, web search price unknown.
fn catalog_without_search_price() -> Catalog {
    Catalog::from_json(&format!(
        r#"{{"entries":[{{"provider":"openai","model":"{MODEL}","input":1,"output":2,"source":"test"}}]}}"#
    ))
    .unwrap()
}

fn seed() -> (Arc<Mutex<Store>>, ArticleId) {
    let mut s = Store::open_in_memory().unwrap();
    let f = s
        .add_feed(&NewFeed { url: "https://news.example/rss".into(), title: "N".into(), ..Default::default() })
        .unwrap();
    s.ingest(
        f,
        &[NewArticle {
            guid: "a".into(),
            url: Some(ARTICLE_URL.into()),
            title: "Kaupunki kasvaa".into(),
            summary: None,
            content: None,
            author: None,
            published_at: Some(1_790_812_800),
        }],
    )
    .unwrap();
    let id = s.conn().query_row("SELECT id FROM articles", [], |r| r.get::<_, i64>(0)).unwrap();
    (Arc::new(Mutex::new(s)), id)
}

fn job(article_id: ArticleId, limit: Micros) -> FactCheckJob {
    FactCheckJob {
        article: ArticleForCheck {
            article_id,
            title: "Kaupunki kasvaa".into(),
            text: format!("{CLAIM_1} {CLAIM_2} {CLAIM_3} Mielestäni tämä on hienoa."),
            url: Some(ARTICLE_URL.into()),
        },
        model: MODEL.into(),
        limit,
    }
}

fn fast() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 2,
        initial_backoff: Duration::from_millis(5),
        max_backoff: Duration::from_millis(20),
        timeout: Duration::from_secs(10),
    }
}

fn client(m: &Mock) -> Client {
    let endpoint = Endpoint { provider: Provider::Openai, base_url: m.base.clone(), auth: Auth::ApiKey("k".into()) };
    Client::new(endpoint, fast()).unwrap()
}

// ---- the fake model ---------------------------------------------------------------

fn is_extract(r: &Recorded) -> bool {
    r.body["text"]["format"]["name"] == "fact_check_claims"
}

fn has_tools(r: &Recorded) -> bool {
    r.body.get("tools").is_some()
}

/// The data block of the request's user message.
fn data_of(r: &Recorded) -> Value {
    let user = r.body["input"].as_array().and_then(|a| a.last()).and_then(|m| m["content"].as_str()).unwrap();
    let block = user.split("<data>\n").nth(1).unwrap().split("\n</data>").next().unwrap();
    serde_json::from_str(block).unwrap()
}

fn claim_of(r: &Recorded) -> String {
    data_of(r)["claim"]["quote"].as_str().unwrap().to_string()
}

/// An OpenAI Responses reply: one search call per entry of `searches`
/// (each with its sources), then the message text.
fn responses(text: &str, searches: &[&[&str]], input: u64, output: u64) -> MockReply {
    let mut out: Vec<Value> = searches
        .iter()
        .map(|urls| {
            let sources: Vec<Value> = urls.iter().map(|u| json!({"type": "url", "url": u, "title": "Found"})).collect();
            json!({"type": "web_search_call", "status": "completed", "action": {"type": "search", "sources": sources}})
        })
        .collect();
    out.push(json!({"type": "message", "content": [{"type": "output_text", "text": text}]}));
    MockReply {
        status: 200,
        body: json!({"status": "completed", "output": out,
            "usage": {"input_tokens": input, "output_tokens": output, "output_tokens_details": {"reasoning_tokens": 0}}}),
        delay: Duration::ZERO,
    }
}

fn extraction(claims: &[&str]) -> MockReply {
    let claims: Vec<Value> = claims.iter().map(|q| json!({"quote": q, "query": "kaupunki asukkaat"})).collect();
    let payload = json!({"language": "fi", "claims": claims, "guidelines": [
        {"guideline": 15, "status": "ok", "reason": "Backed."},
        {"guideline": 12, "status": "concern", "reason": "One source."},
        {"guideline": 21, "status": "not_applicable", "reason": ""},
        {"guideline": 16, "status": "ok", "reason": ""}
    ]});
    responses(&payload.to_string(), &[], 500, 100)
}

fn verdict(verdict: &str, cited: &[&str]) -> String {
    let cited: Vec<Value> = cited.iter().map(|u| json!({"url": u, "same_story": false})).collect();
    json!({"verdict": verdict, "reason": "Checked.", "cited": cited}).to_string()
}

/// Claim calls cost 2000 input and 200 output tokens and one search.
fn claim_reply(text: &str, found: &[&str]) -> MockReply {
    responses(text, &[found], 2_000, 200)
}

// ---- the lookup services ----------------------------------------------------------

async fn lookups(google: Value, wiki: Value, status: u16) -> Mock {
    mock(Arc::new(move |r: &Recorded, _| {
        let body = if r.path.contains("claims:search") { google.clone() } else { wiki.clone() };
        MockReply { status, body, delay: Duration::ZERO }
    }))
    .await
}

async fn empty_lookups() -> Mock {
    lookups(json!({}), json!({"pages": []}), 200).await
}

fn sources(m: &Mock, google: bool) -> Sources {
    Sources::with_bases(google.then(|| "gkey".to_string()), &m.base, &m.base).unwrap()
}

// ---- running ----------------------------------------------------------------------

struct Ran {
    result: Result<FactCheck, FactCheckError>,
    progress: Vec<FactCheckProgress>,
}

async fn run(
    model: &Mock,
    lookup: &Mock,
    google: bool,
    db: &Arc<Mutex<Store>>,
    job: FactCheckJob,
    control: &Control,
) -> Ran {
    run_priced(model, lookup, google, db, job, control, &catalog()).await
}

async fn run_priced(
    model: &Mock,
    lookup: &Mock,
    google: bool,
    db: &Arc<Mutex<Store>>,
    job: FactCheckJob,
    control: &Control,
    catalog: &Catalog,
) -> Ran {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let result = run_fact_check(job, &client(model), &sources(lookup, google), catalog, db.clone(), control, tx).await;
    let mut progress = Vec::new();
    while let Ok(p) = rx.try_recv() {
        progress.push(p);
    }
    Ran { result, progress }
}

fn ledger(db: &Arc<Mutex<Store>>) -> Vec<(String, Option<i64>)> {
    let s = db.lock().unwrap();
    let mut stmt = s.conn().prepare("SELECT operation, cost_microunits FROM usage_ledger ORDER BY id").unwrap();
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect()
}

fn saved(db: &Arc<Mutex<Store>>, id: ArticleId) -> Option<FactCheck> {
    db.lock().unwrap().fact_check(id).unwrap()
}

const PLENTY: Micros = 100_000_000;

/// Saves a check as if from an earlier run.
fn save_earlier(db: &Arc<Mutex<Store>>, id: ArticleId) -> FactCheck {
    let earlier = FactCheck {
        article_id: id,
        checked_at: 1,
        provider: Provider::Openai,
        model: MODEL.into(),
        billing: Billing::ApiKey,
        prompt_version: FACT_CHECK_PROMPT_VERSION,
        language: "fi".into(),
        web_search: true,
        claims: vec![],
        guidelines: vec![],
        cost: Some(1),
    };
    db.lock().unwrap().save_fact_check(&earlier).unwrap();
    earlier
}

// ---- tests ------------------------------------------------------------------------

#[tokio::test]
async fn checks_claims_and_saves_the_result() {
    let m = mock(Arc::new(|r: &Recorded, _| {
        if is_extract(r) {
            return extraction(&[CLAIM_1, CLAIM_2]);
        }
        if claim_of(r) == CLAIM_1 {
            claim_reply(&verdict("contradicted", &[WEB_SOURCE]), &[WEB_SOURCE])
        } else {
            claim_reply(&verdict("supported", &["https://invented.example/x"]), &[WEB_SOURCE])
        }
    }))
    .await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    let ran = run(&m, &l, false, &db, job(id, PLENTY), &Control::default()).await;
    let fc = ran.result.unwrap();

    assert_eq!(fc.claims.len(), 2);
    assert_eq!(fc.claims[0].quote, CLAIM_1);
    assert_eq!(fc.claims[0].verdict, Verdict::Contradicted);
    assert_eq!(fc.claims[0].reason, "Checked.");
    assert_eq!(fc.claims[0].sources.len(), 1);
    assert_eq!(fc.claims[0].sources[0].url, WEB_SOURCE);
    assert_eq!(fc.claims[0].sources[0].kind, SourceKind::Web);
    assert_eq!(fc.claims[1].quote, CLAIM_2);
    assert_eq!(fc.claims[1].verdict, Verdict::Unverified, "an invented URL is not evidence");
    assert_eq!(fc.claims[1].reason, UNCONFIRMED, "the model's reason argued for the dropped verdict");
    assert!(fc.claims[1].sources.is_empty());
    assert_eq!(fc.guidelines.len(), 4);
    assert_eq!(
        (fc.article_id, fc.provider, fc.model.as_str(), fc.billing),
        (id, Provider::Openai, MODEL, Billing::ApiKey)
    );
    assert_eq!((fc.language.as_str(), fc.prompt_version, fc.web_search), ("fi", FACT_CHECK_PROMPT_VERSION, true));
    assert!(fc.checked_at > 0);

    let rows = ledger(&db);
    let ops: Vec<&str> = rows.iter().map(|(o, _)| o.as_str()).collect();
    assert_eq!(ops, ["fact_check_extract", "fact_check_claim", "fact_check_claim"]);
    // Extraction: 500 + 100*2; each claim: 2000 + 200*2 + one search at $0.10.
    assert_eq!(rows.iter().map(|(_, c)| c.unwrap()).collect::<Vec<_>>(), [700, 102_400, 102_400]);
    assert_eq!(fc.cost, Some(700 + 2 * 102_400));
    assert_eq!(saved(&db, id), Some(fc));

    let claim_calls: Vec<Recorded> = m.requests().into_iter().filter(|r| !is_extract(r)).collect();
    assert!(claim_calls.iter().all(|r| has_tools(r) && r.body["max_tool_calls"] == 3));
    assert_eq!(ran.progress.first(), Some(&FactCheckProgress::Extracting));
    assert_eq!(ran.progress.last(), Some(&FactCheckProgress::Checking { done: 2, total: 2 }));
}

#[tokio::test]
async fn opinion_piece_makes_no_claim_calls() {
    let m = mock(Arc::new(|_: &Recorded, _| extraction(&[]))).await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    let fc = run(&m, &l, true, &db, job(id, PLENTY), &Control::default()).await.result.unwrap();
    assert_eq!(m.count(), 1);
    assert_eq!(l.count(), 0);
    assert!(fc.claims.is_empty());
    assert_eq!(fc.guidelines.len(), 4);
    assert_eq!(fc.summary_line(), "No checkable factual claims found.");
    assert_eq!(saved(&db, id), Some(fc));
}

#[tokio::test]
async fn lookup_failures_do_not_fail_the_run() {
    let m = mock(Arc::new(|r: &Recorded, _| {
        if is_extract(r) {
            extraction(&[CLAIM_1])
        } else {
            assert_eq!(data_of(r)["published_fact_checks"], json!([]));
            assert_eq!(data_of(r)["wikipedia"], json!([]));
            claim_reply(&verdict("contradicted", &[WEB_SOURCE]), &[WEB_SOURCE])
        }
    }))
    .await;
    let l = lookups(json!({"error": "boom"}), json!({"error": "boom"}), 500).await;
    let (db, id) = seed();
    let fc = run(&m, &l, true, &db, job(id, PLENTY), &Control::default()).await.result.unwrap();
    assert!(l.count() >= 2, "both lookups were tried");
    assert_eq!(fc.claims[0].verdict, Verdict::Contradicted);
    assert_eq!(fc.claims[0].sources[0].url, WEB_SOURCE);
}

#[tokio::test]
async fn unsupported_web_search_falls_back() {
    let m = mock(Arc::new(|r: &Recorded, _| {
        if is_extract(r) {
            extraction(&[CLAIM_1])
        } else if has_tools(r) {
            MockReply {
                status: 400,
                body: json!({"error": {"message": "Tool 'web_search' is not supported with this model."}}),
                delay: Duration::ZERO,
            }
        } else {
            responses(&verdict("unverified", &[]), &[], 2_000, 200)
        }
    }))
    .await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    let fc = run(&m, &l, false, &db, job(id, PLENTY), &Control::default()).await.result.unwrap();
    let claim_calls: Vec<Recorded> = m.requests().into_iter().filter(|r| !is_extract(r)).collect();
    assert_eq!(claim_calls.len(), 2);
    assert!(has_tools(&claim_calls[0]));
    let resent = &claim_calls[1];
    assert!(!has_tools(resent) && resent.body.get("max_tool_calls").is_none(), "resent without search");
    assert!(!fc.web_search);
    assert_eq!(fc.claims[0].verdict, Verdict::Unverified);
    assert_eq!(fc.claims[0].reason, "Checked.");
    // The rejected request was not billed; the resend has no searches.
    assert_eq!(fc.cost, Some(700 + 2_400));
    assert_eq!(saved(&db, id), Some(fc));
}

#[tokio::test]
async fn malformed_claim_output_becomes_unverified() {
    let m = mock(Arc::new(|r: &Recorded, _| {
        if is_extract(r) {
            extraction(&[CLAIM_1, CLAIM_2])
        } else if claim_of(r) == CLAIM_1 {
            claim_reply("Sorry, I cannot answer that.", &[WEB_SOURCE])
        } else {
            claim_reply(&verdict("contradicted", &[WEB_SOURCE]), &[WEB_SOURCE])
        }
    }))
    .await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    let fc = run(&m, &l, false, &db, job(id, PLENTY), &Control::default()).await.result.unwrap();
    assert_eq!(m.count(), 4, "extraction, the first claim and its retry, the second claim");
    assert_eq!(fc.claims[0].verdict, Verdict::Unverified);
    assert_eq!(fc.claims[0].reason, FAILED);
    assert!(fc.claims[0].sources.is_empty());
    assert_eq!(fc.claims[1].verdict, Verdict::Contradicted);
    assert_eq!(ledger(&db).len(), 4, "both attempts of the first claim are charged");
}

#[tokio::test]
async fn rejected_search_context_size_is_retried_with_full_search() {
    let m = mock(Arc::new(|r: &Recorded, _| {
        if is_extract(r) {
            extraction(&[CLAIM_1, CLAIM_2])
        } else if r.body["tools"][0].get("search_context_size").is_some() {
            MockReply {
                status: 400,
                body: json!({"error": {"message": "Unknown parameter: 'tools[0].search_context_size'."}}),
                delay: Duration::ZERO,
            }
        } else {
            claim_reply(&verdict("supported", &[WEB_SOURCE]), &[WEB_SOURCE])
        }
    }))
    .await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    let fc = run(&m, &l, false, &db, job(id, PLENTY), &Control::default()).await.result.unwrap();
    assert!(fc.web_search, "search stays on");
    assert!(fc.claims.iter().all(|c| c.verdict == Verdict::Supported));
    let claim_calls: Vec<Recorded> = m.requests().into_iter().filter(|r| !is_extract(r)).collect();
    let rejected = claim_calls.iter().filter(|r| r.body["tools"][0].get("search_context_size").is_some()).count();
    assert!((1..=2).contains(&rejected), "each claim in flight may hit the rejection once");
    let last = claim_calls.last().unwrap();
    assert!(has_tools(last) && last.body["tools"][0].get("search_context_size").is_none());
}

#[tokio::test]
async fn limit_marks_remaining_claims_not_checked() {
    let m = mock(Arc::new(|r: &Recorded, _| {
        if is_extract(r) {
            extraction(&[CLAIM_1, CLAIM_2, CLAIM_3])
        } else {
            claim_reply(&verdict("contradicted", &[WEB_SOURCE]), &[WEB_SOURCE])
        }
    }))
    .await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    // A claim's worst case is about $0.34 (three searches at $0.10 plus
    // search result tokens): $0.50 fits the extraction and one claim.
    let fc = run(&m, &l, false, &db, job(id, 500_000), &Control::default()).await.result.unwrap();
    assert_eq!(m.count(), 2, "extraction and one claim");
    let verdicts: Vec<Verdict> = fc.claims.iter().map(|c| c.verdict).collect();
    assert_eq!(verdicts, [Verdict::Contradicted, Verdict::NotChecked, Verdict::NotChecked]);
    assert_eq!(fc.claims[1].quote, CLAIM_2);
    assert_eq!(fc.claims[1].reason, "Not checked (limit reached)");
    assert_eq!(fc.cost, Some(700 + 102_400));
    assert_eq!(fc.summary_line(), "Fact check: 0 of 3 supported · 1 contradicted · 2 not checked");
}

#[tokio::test]
async fn too_low_limit_stops_before_extraction() {
    let m = mock(Arc::new(|_: &Recorded, _| extraction(&[]))).await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    let err = run(&m, &l, false, &db, job(id, 100), &Control::default()).await.result.unwrap_err();
    assert_eq!(err.to_string(), "The fact check limit is too low for this article.");
    assert_eq!(m.count(), 0);
    assert_eq!(saved(&db, id), None);
}

#[tokio::test]
async fn unreadable_extraction_is_retried_once_then_fails() {
    let m = mock(Arc::new(|_: &Recorded, _| responses("not json at all", &[], 500, 100))).await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    let err = run(&m, &l, false, &db, job(id, PLENTY), &Control::default()).await.result.unwrap_err();
    assert_eq!(err.to_string(), "Could not read the article's claims.");
    assert_eq!(m.count(), 2);
    assert_eq!(ledger(&db).len(), 2);
    assert_eq!(saved(&db, id), None);
}

#[tokio::test]
async fn cancel_saves_nothing() {
    let control = Control::default();
    let c = control.clone();
    let m = mock(Arc::new(move |r: &Recorded, _| {
        if is_extract(r) {
            return extraction(&[CLAIM_1, CLAIM_2]);
        }
        c.cancel();
        MockReply { delay: Duration::from_secs(5), ..claim_reply(&verdict("unverified", &[]), &[]) }
    }))
    .await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    let started = std::time::Instant::now();
    let ran = run(&m, &l, false, &db, job(id, PLENTY), &control).await;
    assert!(matches!(ran.result, Err(FactCheckError::Cancelled)));
    assert!(started.elapsed() < Duration::from_secs(3), "cancel does not wait for the provider");
    assert_eq!(saved(&db, id), None);
    let rows = ledger(&db);
    assert_eq!(rows[0].0, "fact_check_extract");
    assert!(rows.len() >= 2, "the claim call in flight is charged: {rows:?}");
    assert!(rows[1..].iter().all(|(op, _)| op == "fact_check_claim:cancelled_in_flight"), "{rows:?}");
}

#[tokio::test]
async fn google_published_rating_reaches_the_card() {
    const CHECK_URL: &str = "https://faktabaari.fi/arkisto/silta";
    let m = mock(Arc::new(|r: &Recorded, _| {
        if is_extract(r) {
            extraction(&[CLAIM_2])
        } else {
            let published = &data_of(r)["published_fact_checks"][0];
            assert_eq!(published["rating"], "Väärä");
            assert!(published["title"].as_str().unwrap().chars().count() <= 301, "lookup text is bounded");
            // The model's search found the same page; the published check's entry wins.
            claim_reply(&verdict("contradicted", &[CHECK_URL]), &[CHECK_URL])
        }
    }))
    .await;
    let google = json!({"claims": [{"text": "Silta maksoi 40 miljoonaa", "claimReview": [{
        "publisher": {"name": "Faktabaari", "site": "faktabaari.fi"}, "url": CHECK_URL,
        "title": format!("Sillan hinta {}", "pitkä ".repeat(200)), "textualRating": "Väärä", "languageCode": "fi"}]}]});
    let wiki = json!({"pages": [{"key": "Silta", "title": "Silta", "excerpt": "Silta on rakennelma."}]});
    let l = lookups(google, wiki, 200).await;
    let (db, id) = seed();
    let fc = run(&m, &l, true, &db, job(id, PLENTY), &Control::default()).await.result.unwrap();
    let c = &fc.claims[0];
    assert_eq!(c.verdict, Verdict::Contradicted);
    assert_eq!(c.sources.len(), 1);
    let s = &c.sources[0];
    assert_eq!((s.url.as_str(), s.kind), (CHECK_URL, SourceKind::FactCheck));
    assert_eq!(s.rating.as_deref(), Some("Väärä"));
    assert_eq!(s.publisher.as_deref(), Some("Faktabaari"));
}

#[tokio::test]
async fn wikipedia_found_by_search_cannot_decide_alone() {
    const WIKI: &str = "https://fi.wikipedia.org/wiki/Silta";
    let m = mock(Arc::new(|r: &Recorded, _| {
        if is_extract(r) { extraction(&[CLAIM_2]) } else { claim_reply(&verdict("supported", &[WIKI]), &[WIKI]) }
    }))
    .await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    let fc = run(&m, &l, false, &db, job(id, PLENTY), &Control::default()).await.result.unwrap();
    assert_eq!(fc.claims[0].verdict, Verdict::Unverified);
    assert_eq!(fc.claims[0].reason, UNCONFIRMED);
    assert_eq!(fc.claims[0].sources[0].kind, SourceKind::Wikipedia);
}

#[tokio::test]
async fn unknown_search_price_checks_without_search() {
    let m = mock(Arc::new(|r: &Recorded, _| {
        if is_extract(r) {
            extraction(&[CLAIM_1, CLAIM_2])
        } else {
            assert!(!has_tools(r), "no search is sent when its price is unknown");
            responses(&verdict("unverified", &[]), &[], 2_000, 200)
        }
    }))
    .await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    let ran =
        run_priced(&m, &l, false, &db, job(id, PLENTY), &Control::default(), &catalog_without_search_price()).await;
    let fc = ran.result.unwrap();
    assert!(!fc.web_search);
    assert_eq!(m.count(), 3);
    assert!(fc.claims.iter().all(|c| c.verdict == Verdict::Unverified && c.reason == "Checked."), "{:?}", fc.claims);
    assert_eq!(fc.cost, Some(700 + 2 * 2_400));
}

#[tokio::test]
async fn unknown_model_price_fails_up_front() {
    let m = mock(Arc::new(|_: &Recorded, _| extraction(&[]))).await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    let empty = Catalog::from_json(r#"{"entries":[]}"#).unwrap();
    let err = run_priced(&m, &l, false, &db, job(id, PLENTY), &Control::default(), &empty).await.result.unwrap_err();
    assert!(err.to_string().contains("price of this model is unknown"), "{err}");
    assert_eq!(m.count(), 0);
}

#[tokio::test]
async fn account_error_on_a_claim_fails_the_run_and_keeps_the_earlier_check() {
    let m = mock(Arc::new(|r: &Recorded, _| {
        if is_extract(r) {
            extraction(&[CLAIM_1, CLAIM_2, CLAIM_3])
        } else {
            MockReply {
                status: 429,
                body: json!({"error": {"type": "insufficient_quota", "code": "insufficient_quota",
                    "message": "You exceeded your current quota."}}),
                delay: Duration::ZERO,
            }
        }
    }))
    .await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    let earlier = save_earlier(&db, id);
    let err = run(&m, &l, false, &db, job(id, PLENTY), &Control::default()).await.result.unwrap_err();
    assert!(matches!(err, FactCheckError::Ai(_)));
    assert!(err.to_string().contains("no credit left"), "{err}");
    assert_eq!(saved(&db, id), Some(earlier), "a failed run does not replace the earlier check");
    assert_eq!(ledger(&db)[0].0, "fact_check_extract");
}

#[tokio::test]
async fn refused_retry_after_malformed_output_is_unverified() {
    let m = mock(Arc::new(|r: &Recorded, _| {
        if is_extract(r) { extraction(&[CLAIM_1, CLAIM_2]) } else { claim_reply("not json", &[WEB_SOURCE]) }
    }))
    .await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    // $0.40 fits the first claim attempt (about $0.34) but not a second one
    // after the first was charged, nor the second claim.
    let fc = run(&m, &l, false, &db, job(id, 400_000), &Control::default()).await.result.unwrap();
    assert_eq!(m.count(), 2, "extraction and one claim attempt");
    assert_eq!(fc.claims[0].verdict, Verdict::Unverified);
    assert_eq!(fc.claims[0].reason, FAILED);
    assert_eq!(fc.claims[1].verdict, Verdict::NotChecked, "a claim stopped by the limit counts as judged");
}

#[tokio::test]
async fn reasoning_model_gets_room_to_think() {
    let m = mock(Arc::new(|r: &Recorded, _| {
        if is_extract(r) {
            extraction(&[CLAIM_1])
        } else {
            claim_reply(&verdict("contradicted", &[WEB_SOURCE]), &[WEB_SOURCE])
        }
    }))
    .await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    let reasoning = Catalog::from_json(&format!(
        r#"{{"entries":[{{"provider":"openai","model":"{MODEL}","input":1,"output":2,"web_search_per_1k":100,"reasoning":true,"source":"test"}}]}}"#
    ))
    .unwrap();
    run_priced(&m, &l, false, &db, job(id, PLENTY), &Control::default(), &reasoning).await.result.unwrap();
    let caps: Vec<(bool, Value)> =
        m.requests().iter().map(|r| (is_extract(r), r.body["max_output_tokens"].clone())).collect();
    assert_eq!(caps, [(true, json!(8000)), (false, json!(6000))]);
    // A model without reasoning keeps the smaller caps.
    let m2 = mock(Arc::new(|r: &Recorded, _| {
        if is_extract(r) { extraction(&[CLAIM_1]) } else { claim_reply(&verdict("unverified", &[]), &[]) }
    }))
    .await;
    let (db, id) = seed();
    run(&m2, &l, false, &db, job(id, PLENTY), &Control::default()).await.result.unwrap();
    let caps: Vec<Value> = m2.requests().iter().map(|r| r.body["max_output_tokens"].clone()).collect();
    assert_eq!(caps, [json!(4000), json!(1500)]);
}

#[tokio::test]
async fn a_run_where_every_claim_failed_keeps_the_earlier_check() {
    let m = mock(Arc::new(|r: &Recorded, _| {
        if is_extract(r) {
            extraction(&[CLAIM_1, CLAIM_2])
        } else if claim_of(r) == CLAIM_1 {
            MockReply { status: 500, body: json!({"error": {"message": "server exploded"}}), delay: Duration::ZERO }
        } else {
            claim_reply("not json", &[WEB_SOURCE])
        }
    }))
    .await;
    let l = empty_lookups().await;
    let (db, id) = seed();
    let earlier = save_earlier(&db, id);
    let err = run(&m, &l, false, &db, job(id, PLENTY), &Control::default()).await.result.unwrap_err();
    assert!(matches!(err, FactCheckError::Ai(_)), "{err:?}");
    assert_eq!(err.to_string(), "The fact check could not be completed. Your earlier result, if any, was kept.");
    assert_eq!(saved(&db, id), Some(earlier), "a run with no judged claim does not replace the earlier check");
}

#[tokio::test]
async fn cancel_during_lookups_saves_nothing_and_charges_no_claim() {
    let control = Control::default();
    let c = control.clone();
    let m = mock(Arc::new(|r: &Recorded, _| {
        if is_extract(r) {
            extraction(&[CLAIM_1, CLAIM_2])
        } else {
            claim_reply(&verdict("contradicted", &[WEB_SOURCE]), &[WEB_SOURCE])
        }
    }))
    .await;
    let l = mock(Arc::new(move |_: &Recorded, _| {
        c.cancel();
        MockReply { status: 200, body: json!({"pages": []}), delay: Duration::from_secs(5) }
    }))
    .await;
    let (db, id) = seed();
    let started = std::time::Instant::now();
    let ran = run(&m, &l, true, &db, job(id, PLENTY), &control).await;
    assert!(matches!(ran.result, Err(FactCheckError::Cancelled)), "{:?}", ran.result.map(|_| ()));
    assert!(started.elapsed() < Duration::from_secs(3), "cancel does not wait for the lookups");
    assert_eq!(m.count(), 1, "no claim call was sent");
    // The claims' reservations were released, not charged.
    assert_eq!(ledger(&db).iter().map(|(op, _)| op.as_str()).collect::<Vec<_>>(), ["fact_check_extract"]);
    assert_eq!(saved(&db, id), None);
}

/// The app runs a check on a spawned task.
#[allow(dead_code)]
fn run_is_send(job: FactCheckJob, c: &Client, s: &Sources, cat: &Catalog, db: Arc<Mutex<Store>>, ctl: &Control) {
    fn is_send<T: Send>(_: T) {}
    let (tx, _rx) = mpsc::unbounded_channel();
    is_send(run_fact_check(job, c, s, cat, db, ctl, tx));
}

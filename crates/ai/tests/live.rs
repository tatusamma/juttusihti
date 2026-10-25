//! Opt-in live API tests. They are ignored by default and never run in
//! normal test runs. Each sends one tiny request (a single made-up headline)
//! and costs a fraction of a cent.
//!
//! Run one provider:
//!   JUTTUSIHTI_LIVE_ANTHROPIC_KEY=... cargo test -p juttusihti-ai --test live -- --ignored anthropic
//!   JUTTUSIHTI_LIVE_OPENAI_KEY=...    cargo test -p juttusihti-ai --test live -- --ignored openai
//!   JUTTUSIHTI_LIVE_GEMINI_KEY=...    cargo test -p juttusihti-ai --test live -- --ignored gemini
//! Optional JUTTUSIHTI_LIVE_MODEL overrides the model.

use std::collections::BTreeSet;

use juttusihti_ai::prompts::{extraction_request, validate_extraction};
use juttusihti_ai::{Client, Endpoint, Outcome, RetryPolicy};
use juttusihti_core::analysis::{ArticleRecord, Task};
use juttusihti_core::pricing::Provider;

fn record() -> ArticleRecord {
    ArticleRecord {
        id: 1,
        feed_id: 1,
        effective_at: 0,
        title: "Helsinki city council approves new tram line to Malmi".into(),
        body: "The council voted 45 to 20 on Tuesday.".into(),
        truncated: false,
        content_hash: String::new(),
        dedup_key: String::new(),
    }
}

async fn live(provider: Provider, env: &str, default_model: &str) {
    let Ok(key) = std::env::var(env) else {
        eprintln!("{env} not set; skipping");
        return;
    };
    let model = std::env::var("JUTTUSIHTI_LIVE_MODEL").unwrap_or_else(|_| default_model.into());
    let client = Client::new(Endpoint::official(provider, key), RetryPolicy::default()).unwrap();
    let tasks: BTreeSet<Task> = [Task::Labels, Task::Entities].into_iter().collect();
    let rec = record();
    let req = extraction_request(&model, &tasks, &[&rec], &[], None, 1_024);

    let counted = client.count_tokens(&req).await.expect("token counting");
    let local = juttusihti_core::estimate::approx_tokens(&req.system)
        + juttusihti_core::estimate::approx_tokens(&req.user)
        + juttusihti_core::estimate::approx_tokens(&req.schema.to_string());
    eprintln!("{provider:?} {model}: provider count {counted}, local estimate {local}");
    assert!(counted > 0);

    let reply = client.complete(&req).await.expect("completion");
    eprintln!("usage {:?}, reasoning {}", reply.usage, reply.reasoning_tokens);
    match reply.outcome {
        Outcome::Json(v) => {
            let items = validate_extraction(&v, &[&rec], &tasks).expect("valid output");
            eprintln!("{items:?}");
            assert_eq!(items.len(), 1);
        }
        other => panic!("unexpected outcome {other:?}"),
    }
}

#[tokio::test]
#[ignore = "calls the real Anthropic API; needs JUTTUSIHTI_LIVE_ANTHROPIC_KEY"]
async fn anthropic() {
    live(Provider::Anthropic, "JUTTUSIHTI_LIVE_ANTHROPIC_KEY", "claude-haiku-5-5").await;
}

#[tokio::test]
#[ignore = "calls the real OpenAI API; needs JUTTUSIHTI_LIVE_OPENAI_KEY"]
async fn openai() {
    live(Provider::Openai, "JUTTUSIHTI_LIVE_OPENAI_KEY", "gpt-6-luna").await;
}

#[tokio::test]
#[ignore = "calls the real Gemini API; needs JUTTUSIHTI_LIVE_GEMINI_KEY"]
async fn gemini() {
    live(Provider::Gemini, "JUTTUSIHTI_LIVE_GEMINI_KEY", "gemini-3.8-flash").await;
}

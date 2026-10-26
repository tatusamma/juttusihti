use std::collections::BTreeSet;

use juttusihti_core::analysis::{AnalysisConfig, Depth, Task, cache_key};
use juttusihti_core::budget::Limits;
use juttusihti_core::estimate::{Scenario, Stage, estimate};
use juttusihti_core::pricing::{Catalog, Provider};
use juttusihti_core::search::SearchQuery;
use juttusihti_core::store::Store;
use juttusihti_core::*;

const OCT1: i64 = 1_790_812_800; // 2026-10-01T00:00:00Z
const DAY: i64 = 86_400;

fn today() -> chrono::NaiveDate {
    "2026-10-25".parse().unwrap()
}

fn entry(guid: &str, url: &str, title: &str, summary: Option<&str>, at: Option<i64>) -> NewArticle {
    NewArticle {
        guid: guid.into(),
        url: Some(url.into()),
        title: title.into(),
        summary: summary.map(str::to_string),
        content: None,
        author: None,
        published_at: at,
    }
}

fn seeded() -> Store {
    let mut s = Store::open_in_memory().unwrap();
    let a =
        s.add_feed(&NewFeed { url: "https://a.example/rss".into(), title: "A".into(), ..Default::default() }).unwrap();
    let b =
        s.add_feed(&NewFeed { url: "https://b.example/rss".into(), title: "B".into(), ..Default::default() }).unwrap();
    s.ingest(
        a,
        &[
            entry(
                "1",
                "https://x/1",
                "Hallitus esittää muutoksia työttömyysturvaan",
                Some("Esitys koskee päivärahaa."),
                Some(OCT1),
            ),
            entry(
                "2",
                "https://x/2",
                "Lumisade jatkuu Lapissa",
                Some("Ilmatieteen laitos varoittaa."),
                Some(OCT1 + DAY),
            ),
            entry("3", "https://x/3", "Undated item", None, None),
            entry("4", "https://x/4", "Old news outside range", Some("x"), Some(OCT1 - 10 * DAY)),
        ],
    )
    .unwrap();
    // Syndicated copy of article 1 in another feed: same URL.
    s.ingest(
        b,
        &[
            entry(
                "s1",
                "https://x/1",
                "Hallitus esittää muutoksia työttömyysturvaan (STT)",
                Some("Esitys koskee päivärahaa."),
                Some(OCT1 + 60),
            ),
            entry(
                "5",
                "https://x/5",
                "Hallituksen esitys työttömyysturvasta herättää kritiikkiä",
                Some("Oppositio arvostelee."),
                Some(OCT1 + 2 * DAY),
            ),
        ],
    )
    .unwrap();
    s
}

fn config(tasks: &[Task]) -> AnalysisConfig {
    AnalysisConfig {
        scope: SearchQuery { from: Some(OCT1), to: Some(OCT1 + 31 * DAY), ..Default::default() },
        range_label: "1.10.2026 – 31.10.2026".into(),
        provider: Provider::Anthropic,
        model: "claude-haiku-5-5".into(),
        depth: Depth::HeadlinesAndDescriptions,
        tasks: tasks.iter().copied().collect::<BTreeSet<_>>(),
        limits: Limits { max_tokens: Some(1_000_000), max_cost: Some(1_000_000), ..Default::default() },
        max_articles: 1_000,
        reuse_cached: true,
        use_batch: false,
        relevance_criteria: None,
    }
}

#[test]
fn scope_dedups_and_respects_ranges() {
    let s = seeded();
    let scope = s.analysis_scope(&config(&[Task::Labels])).unwrap();
    // In range: 1, 2, s1, 5, plus the undated article (falls back to fetch time = now, outside Oct... unless
    // fetched in range). Fetch time is "now" (2026-10-25 or later), so include it only if before Nov 1.
    let undated_in_range = now() < OCT1 + 31 * DAY;
    assert_eq!(scope.stats.matched, 4 + u64::from(undated_in_range));
    assert_eq!(scope.stats.local_duplicates, 1, "syndicated copy collapses");
    assert_eq!(scope.stats.distinct, scope.stats.matched - 1);
    assert_eq!(scope.stats.to_process, scope.stats.distinct);

    let mut dated = config(&[Task::Labels]);
    dated.scope.require_published_date = true;
    assert_eq!(s.analysis_scope(&dated).unwrap().stats.matched, 4);

    let mut capped = config(&[Task::Labels]);
    capped.max_articles = 2;
    let c = s.analysis_scope(&capped).unwrap();
    assert_eq!(c.stats.selected, 2);
    let e = estimate(&capped, &c, 0, &Catalog::builtin(), today());
    assert!(e.warnings.iter().any(|w| w.contains("maximum")), "{:?}", e.warnings);
}

#[test]
fn cached_results_are_not_counted_again() {
    let s = seeded();
    let cfg = config(&[Task::Labels, Task::Entities]);
    let scope = s.analysis_scope(&cfg).unwrap();
    let first = &scope.records[0];
    for t in [Task::Labels, Task::Entities] {
        let key = cache_key(&first.content_hash, cfg.provider, &cfg.model, t, cfg.depth);
        s.cache_result(&key, first.id, t, &serde_json::json!({"ok": true})).unwrap();
    }
    let again = s.analysis_scope(&cfg).unwrap();
    assert_eq!(again.stats.cached, 1);
    assert_eq!(again.stats.to_process, scope.stats.to_process - 1);
    assert!(!again.needs_processing.contains(&first.id));

    // Only partially cached (labels but not relevance) still needs work.
    let partial = s.analysis_scope(&config(&[Task::Labels, Task::Relevance])).unwrap();
    assert_eq!(partial.stats.cached, 0);
    // A different model never reuses the cache.
    let mut other = cfg.clone();
    other.model = "claude-opus-5-5".into();
    assert_eq!(s.analysis_scope(&other).unwrap().stats.cached, 0);
    // Reuse can be turned off.
    let mut fresh = cfg.clone();
    fresh.reuse_cached = false;
    assert_eq!(s.analysis_scope(&fresh).unwrap().stats.cached, 0);
}

#[test]
fn related_task_uses_local_candidates_only() {
    let s = seeded();
    let scope = s.analysis_scope(&config(&[Task::Related])).unwrap();
    assert_eq!(scope.candidates.len(), 1, "{:?}", scope.candidates);
    let e = estimate(&config(&[Task::Related]), &scope, 0, &Catalog::builtin(), today());
    assert_eq!(e.requests, 1);
    assert_eq!(e.stages[0].stage, Stage::RelationshipValidation);
}

#[test]
fn estimate_has_ordered_scenarios_and_known_cost() {
    let s = seeded();
    let cfg = config(&[Task::Labels, Task::Entities, Task::Relevance, Task::Summary]);
    let scope = s.analysis_scope(&cfg).unwrap();
    let e = estimate(&cfg, &scope, 25, &Catalog::builtin(), today());
    assert!(e.price_known);
    assert_eq!(e.requests, 2, "one extraction batch + one summary");
    let opt = &e.scenarios[&Scenario::Optimistic];
    let exp = e.expected();
    let con = e.conservative();
    assert!(opt.usage.total() <= exp.usage.total() && exp.usage.total() <= con.usage.total());
    assert!(opt.cost.unwrap() <= exp.cost.unwrap() && exp.cost.unwrap() <= con.cost.unwrap());
    assert!(e.worst_case_tokens >= con.usage.total());
    assert!(e.price_source.as_deref().unwrap().contains("platform.claude.com"));

    // Runs are sent as normal requests and charged at standard prices, so a
    // (legacy) batch flag must not discount the estimate.
    let mut batch = cfg.clone();
    batch.use_batch = true;
    let b = estimate(&batch, &scope, 25, &Catalog::builtin(), today());
    assert_eq!(b.expected().cost, e.expected().cost, "estimate must match what a run is charged");
    assert_eq!(b.conservative().cost, e.conservative().cost);
    assert!(b.warnings.iter().any(|w| w.contains("Batch processing is not supported")), "{:?}", b.warnings);
}

#[test]
fn unknown_model_price_is_unknown_and_warned() {
    let s = seeded();
    let mut cfg = config(&[Task::Labels]);
    cfg.provider = Provider::Openai;
    cfg.model = "not-in-catalog".into();
    let scope = s.analysis_scope(&cfg).unwrap();
    let e = estimate(&cfg, &scope, 0, &Catalog::builtin(), today());
    assert!(!e.price_known);
    assert_eq!(e.expected().cost, None);
    assert!(e.warnings.iter().any(|w| w.contains("No price is known")));
}

#[test]
fn budget_warnings_and_depth_warnings() {
    let s = seeded();
    let mut cfg = config(&[Task::Summary, Task::Entities]);
    cfg.limits.max_tokens = Some(10);
    cfg.limits.max_cost = Some(1);
    cfg.limits.max_calls = Some(1);
    cfg.depth = Depth::Headlines;
    let scope = s.analysis_scope(&cfg).unwrap();
    let e = estimate(&cfg, &scope, 0, &Catalog::builtin(), today());
    let all = e.warnings.join("\n");
    assert!(all.contains("token limit"), "{all}");
    assert!(all.contains("cost limit"), "{all}");
    assert!(all.contains("call limit"), "{all}");
    assert!(all.contains("Headlines alone"), "{all}");
}

#[test]
fn estimated_session_snapshots_scope_and_awaits_approval() {
    let mut s = seeded();
    let cfg = config(&[Task::Labels]);
    let scope = s.analysis_scope(&cfg).unwrap();
    let e = estimate(&cfg, &scope, 0, &Catalog::builtin(), today());
    let id = s.save_estimated_session(&cfg, &scope, &e).unwrap();
    let rec = s.session(id).unwrap();
    assert_eq!(rec.status, SessionStatus::AwaitingApproval);
    let back: AnalysisConfig = serde_json::from_value(rec.config).unwrap();
    assert_eq!(back, cfg);
    let est: juttusihti_core::estimate::Estimate = serde_json::from_value(rec.estimate.unwrap()).unwrap();
    assert_eq!(est, e);
    assert_eq!(s.session_articles(id).unwrap().len(), scope.selected_ids.len());
    let list = s.recent_sessions(10).unwrap();
    assert_eq!(list[0].id, id);
    assert_eq!(list[0].range_label, "1.10.2026 – 31.10.2026");
}

#[test]
fn ledger_totals_feed_daily_budgets() {
    let s = seeded();
    s.conn()
        .execute_batch(&format!(
            "INSERT INTO usage_ledger(provider, model, operation, input_tokens, output_tokens, cost_microunits, created_at)
             VALUES ('anthropic','m','labels',100,50,700,{t}), ('openai','m','labels',10,5,NULL,{t}), ('openai','m','x',1,1,1,0);",
            t = OCT1
        ))
        .unwrap();
    let t = s.usage_since(OCT1).unwrap();
    assert_eq!((t.tokens, t.cost, t.calls), (165, 700, 2));
    assert!(t.cost_unknown);
}

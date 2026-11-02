use std::collections::BTreeSet;

use juttusihti_core::analysis::{AnalysisConfig, ClickbaitCheck, Depth, Task, cache_key, clickbait_key};
use juttusihti_core::budget::Limits;
use juttusihti_core::estimate::{Scenario, Stage, estimate};
use juttusihti_core::pricing::{Billing, Catalog, Provider};
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
        billing: Default::default(),
        relevance_criteria: None,
        clickbait: juttusihti_core::analysis::ClickbaitCheck::off(),
        parallel_requests: 4,
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
fn clickbait_check_plans_its_own_stage_and_settles_headlines() {
    let s = seeded();
    let mut cfg = config(&[Task::Headlines]);
    (cfg.provider, cfg.model, cfg.clickbait) = (Provider::Openai, "gpt-6-luna".into(), ClickbaitCheck::default());
    assert!(cfg.clickbait_check().is_some());
    let scope = s.analysis_scope(&cfg).unwrap();
    // Three distinct articles in range have a description; one does not
    // and needs nothing, as no rewrite could be grounded.
    assert_eq!(scope.needs_processing.len(), 3);
    let e = estimate(&cfg, &scope, 0, &Catalog::builtin(), today());
    assert_eq!(e.stages[0].stage, Stage::ClickbaitCheck);
    assert_eq!((e.stages[0].requests, e.stages[0].expected_output), (1, 0), "one batch; output is not charged");
    assert_eq!(e.stages[1].stage, Stage::Extraction, "the rewrite stage stays an upper bound");
    assert!(e.price_known);
    assert!(e.warnings.iter().any(|w| w.contains("clickbait check")), "{:?}", e.warnings);
    assert!(e.warnings.iter().all(|w| !w.contains("  ")), "no runs of spaces: {:?}", e.warnings);

    // A cached probability below the threshold settles the headline task;
    // one at the threshold keeps it open, and so does a lower threshold.
    let (low, high) = (&scope.records[0], &scope.records[1]);
    let key = |r: &juttusihti_core::analysis::ArticleRecord| clickbait_key(&r.content_hash, &cfg.clickbait, cfg.depth);
    s.cache_result(&key(low), low.id, Task::Headlines, &serde_json::json!({"probability": 0.2})).unwrap();
    s.cache_result(&key(high), high.id, Task::Headlines, &serde_json::json!({"probability": 0.7})).unwrap();
    assert_eq!(s.cached_clickbait(&key(low)).unwrap(), Some(0.2));
    assert!(s.open_tasks(&cfg, low, true).unwrap().is_empty());
    assert!(s.open_tasks(&cfg, high, true).unwrap().contains(&Task::Headlines));
    assert!(s.open_tasks(&cfg, low, false).unwrap().contains(&Task::Headlines), "reuse can be turned off");
    let mut strict = cfg.clone();
    strict.clickbait.threshold_percent = 15;
    assert!(s.open_tasks(&strict, low, true).unwrap().contains(&Task::Headlines));
    assert_eq!(s.analysis_scope(&cfg).unwrap().needs_processing.len(), 2);

    // Not on other providers, on a ChatGPT plan, or with headlines only.
    for change in [
        |c: &mut AnalysisConfig| c.provider = Provider::Anthropic,
        |c: &mut AnalysisConfig| c.billing = Billing::ChatGptPlan,
        |c: &mut AnalysisConfig| c.depth = Depth::Headlines,
        |c: &mut AnalysisConfig| c.clickbait.enabled = false,
    ] {
        let mut other = cfg.clone();
        change(&mut other);
        assert!(other.clickbait_check().is_none());
        let scope = s.analysis_scope(&other).unwrap();
        let e = estimate(&other, &scope, 0, &Catalog::builtin(), today());
        assert!(e.stages.iter().all(|st| st.stage != Stage::ClickbaitCheck));
    }
}

#[test]
fn a_plan_run_can_pay_for_the_clickbait_check_with_api_credit() {
    let s = seeded();
    let mut cfg = config(&[Task::Headlines]);
    (cfg.provider, cfg.model, cfg.billing) = (Provider::Openai, "model-only-on-the-plan".into(), Billing::ChatGptPlan);
    cfg.clickbait = ClickbaitCheck::default();
    assert!(cfg.clickbait_check().is_none(), "plan runs have no check unless asked");
    assert!(!cfg.check_on_api_credit());
    let scope = s.analysis_scope(&cfg).unwrap();
    assert_eq!(estimate(&cfg, &scope, 0, &Catalog::builtin(), today()).expected().cost, Some(0));

    cfg.clickbait.on_api_credit = true;
    assert!(cfg.clickbait_check().is_some());
    assert!(cfg.check_on_api_credit());
    let e = estimate(&cfg, &scope, 0, &Catalog::builtin(), today());
    assert_eq!(e.stages[0].stage, Stage::ClickbaitCheck);
    assert!(e.price_known);
    assert!(e.expected().cost.unwrap() > 0, "the check is real money; the plan part is a known zero");
    assert!(e.warnings.iter().any(|w| w.contains("paid with API credit")), "{:?}", e.warnings);
    assert!(e.warnings.iter().all(|w| !w.contains("  ")), "no runs of spaces: {:?}", e.warnings);

    // The cached ratings stay valid whichever way the check is paid for.
    let mut on_key = cfg.clickbait.clone();
    on_key.on_api_credit = false;
    assert_eq!(clickbait_key("h", &cfg.clickbait, cfg.depth), clickbait_key("h", &on_key, cfg.depth));

    // An API-key run is unaffected, and so is one that turned the check off.
    let mut key = cfg.clone();
    key.billing = Billing::ApiKey;
    assert!(key.clickbait_check().is_some() && !key.check_on_api_credit());
    cfg.clickbait.enabled = false;
    assert!(!cfg.check_on_api_credit());
    // So are other providers.
    cfg.clickbait.enabled = true;
    cfg.provider = Provider::Anthropic;
    assert!(cfg.clickbait_check().is_none());
}

#[test]
fn sessions_saved_before_the_split_stay_without_it() {
    let mut cfg = config(&[Task::Headlines]);
    (cfg.provider, cfg.billing, cfg.clickbait) = (Provider::Openai, Billing::ChatGptPlan, ClickbaitCheck::default());
    let mut v = serde_json::to_value(&cfg).unwrap();
    v["clickbait"].as_object_mut().unwrap().remove("on_api_credit");
    let old: AnalysisConfig = serde_json::from_value(v).unwrap();
    assert!(!old.clickbait.on_api_credit);
    assert!(old.clickbait_check().is_none(), "approved without a check, runs without one");
}

#[test]
fn sessions_saved_before_the_clickbait_check_run_without_it() {
    let mut v = serde_json::to_value(config(&[Task::Headlines])).unwrap();
    v.as_object_mut().unwrap().remove("clickbait");
    v.as_object_mut().unwrap().remove("parallel_requests");
    let old: AnalysisConfig = serde_json::from_value(v).unwrap();
    assert!(!old.clickbait.enabled);
    assert_eq!(old.parallel_requests(), 4);
    assert_eq!(ClickbaitCheck::default().threshold(), 0.70);
    assert_eq!(ClickbaitCheck { batch_size: 10_000, ..Default::default() }.batch_size(), 400);
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
fn chatgpt_plan_has_zero_api_cost_and_a_warning() {
    let s = seeded();
    let mut cfg = config(&[Task::Labels]);
    cfg.provider = Provider::Openai;
    cfg.model = "model-only-on-the-plan".into();
    cfg.billing = Billing::ChatGptPlan;
    let scope = s.analysis_scope(&cfg).unwrap();
    let e = estimate(&cfg, &scope, 0, &Catalog::builtin(), today());
    assert!(e.price_known, "plan billing is a known zero, not an unknown price");
    assert_eq!(e.conservative().cost, Some(0));
    assert!(e.expected().usage.total() > 0, "tokens are still estimated");
    assert!(e.price_source.as_deref().unwrap().contains("ChatGPT plan"));
    assert!(e.warnings.iter().any(|w| w.contains("ChatGPT plan allowance")), "{:?}", e.warnings);

    // Only OpenAI can bill a ChatGPT plan.
    cfg.provider = Provider::Anthropic;
    let e = estimate(&cfg, &scope, 0, &Catalog::builtin(), today());
    assert!(!e.price_known);
    assert!(e.warnings.iter().any(|w| w.contains("cannot be billed to a ChatGPT plan")), "{:?}", e.warnings);
}

#[test]
fn sessions_saved_before_billing_existed_load_as_api_key() {
    let mut v = serde_json::to_value(config(&[Task::Labels])).unwrap();
    v.as_object_mut().unwrap().remove("billing");
    let back: AnalysisConfig = serde_json::from_value(v).unwrap();
    assert_eq!(back.billing, Billing::ApiKey);
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

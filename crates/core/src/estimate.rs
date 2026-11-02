//! Offline token and cost estimation for an analysis run.
//!
//! The plan lists every request the run would make, stage by stage, and each
//! request is priced on its own so long-context tiers apply correctly. Token
//! counts come from a local approximation (no network); provider token
//! counting can refine them later with the user's consent. Results are
//! estimates with a range, never invoices.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::analysis::{AnalysisConfig, Depth, Scope, Task};
use crate::pricing::{Billing, Catalog, Micros, Price, Provider, Usage, format_usd};

/// Local token approximation for one text. Calibrated loosely: ASCII text
/// averages ~4 characters per token across current tokenizers; non-ASCII
/// letters (ä, ö, å, other scripts) split far more often. Treat the result as
/// the "expected" value; scenarios widen it.
pub fn approx_tokens(text: &str) -> u64 {
    let (mut ascii, mut other) = (0u64, 0u64);
    for c in text.chars() {
        if c.is_ascii() { ascii += 1 } else { other += 1 }
    }
    (ascii * 10).div_ceil(38) + (other * 10).div_ceil(16)
}

/// Fixed prompt overhead per request: instructions, output schema and
/// injection-resistance framing. Measured from the prompt templates once
/// they exist; until then a conservative constant.
const REQUEST_OVERHEAD: u64 = 900;
/// Extra tokens per article for the record wrapper and ids.
const RECORD_OVERHEAD: u64 = 12;
/// Tokens per existing label sent as vocabulary.
const TOKENS_PER_LABEL: u64 = 5;

const EXTRACT_BATCH: usize = 20;
const PAIR_BATCH: usize = 10;
const SUMMARY_CHUNK: usize = 40;

/// Clickbait check: fixed criteria per request, and per headline its question.
const CHECK_OVERHEAD: u64 = 400;
const CHECK_QUESTION: u64 = 30;

const OUT_LABELS: u64 = 30;
const OUT_ENTITIES: u64 = 90;
const OUT_RELEVANCE: u64 = 25;
const OUT_HEADLINE: u64 = 30;
const OUT_PAIR: u64 = 50;
const OUT_CHUNK_SUMMARY: u64 = 500;
const OUT_FINAL_SUMMARY: u64 = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Decisions API clickbait check before headline rewriting.
    ClickbaitCheck,
    Extraction,
    RelationshipValidation,
    ChunkSummaries,
    FinalSummary,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::ClickbaitCheck => "Clickbait check",
            Stage::Extraction => "Labels / entities / relevance",
            Stage::RelationshipValidation => "Related-story validation",
            Stage::ChunkSummaries => "Partial summaries",
            Stage::FinalSummary => "Final summary",
        }
    }
}

/// One planned request with its expected sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedRequest {
    pub stage: Stage,
    /// Fixed instructions part, cacheable across requests of a stage.
    pub overhead_tokens: u64,
    pub content_tokens: u64,
    pub expected_output: u64,
    /// The `max_tokens` the request would allow; used for budget reservation.
    pub max_output: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    Optimistic,
    Expected,
    Conservative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioTotals {
    pub usage: Usage,
    /// `None` when the model's pricing is unknown or incomplete.
    pub cost: Option<Micros>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageTotals {
    pub stage: Stage,
    pub requests: u64,
    pub expected_input: u64,
    pub expected_output: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Estimate {
    pub stats: crate::analysis::ScopeStats,
    pub candidate_pairs: u64,
    pub stages: Vec<StageTotals>,
    pub requests: u64,
    pub scenarios: BTreeMap<Scenario, ScenarioTotals>,
    /// Sum over requests of prompt + `max_tokens`: the most the run can use
    /// if every call hits its output limit.
    pub worst_case_tokens: u64,
    pub price_known: bool,
    pub price_source: Option<String>,
    pub warnings: Vec<String>,
}

impl Estimate {
    pub fn expected(&self) -> &ScenarioTotals {
        &self.scenarios[&Scenario::Expected]
    }
    pub fn conservative(&self) -> &ScenarioTotals {
        &self.scenarios[&Scenario::Conservative]
    }
}

/// `max_tokens` for a request: room above the conservative scenario's
/// output allowance (4× expected for reasoning models) plus headroom.
fn max_output(expected: u64) -> u64 {
    expected * 5 + 1_024
}

/// Builds the request plan for a scope.
pub fn plan(config: &AnalysisConfig, scope: &Scope, label_vocabulary: usize) -> Vec<PlannedRequest> {
    let mut out = Vec::new();
    let by_id: std::collections::HashMap<i64, &crate::analysis::ArticleRecord> =
        scope.records.iter().map(|r| (r.id, r)).collect();
    let record_tokens = |id: &i64| by_id.get(id).map_or(0, |r| approx_tokens(&r.prompt_text()) + RECORD_OVERHEAD);

    // Stage 0: the clickbait check classifies headlines that could be
    // rewritten. The Decisions API does not charge output tokens. The
    // extraction stage below still assumes every headline is rewritten, so
    // the estimate stays an upper bound.
    if let Some(check) = config.clickbait_check() {
        let with_text: Vec<i64> = scope
            .needs_processing
            .iter()
            .copied()
            .filter(|id| by_id.get(id).is_some_and(|r| !r.body.trim().is_empty()))
            .collect();
        for chunk in with_text.chunks(check.batch_size()) {
            out.push(PlannedRequest {
                stage: Stage::ClickbaitCheck,
                overhead_tokens: CHECK_OVERHEAD,
                content_tokens: chunk.iter().map(|id| record_tokens(id) + CHECK_QUESTION).sum(),
                expected_output: 0,
                max_output: 0,
            });
        }
    }

    // Stage 1: one combined pass for all per-article tasks.
    let per_article_out: u64 = config
        .tasks
        .iter()
        .map(|t| match t {
            Task::Labels => OUT_LABELS,
            Task::Entities => OUT_ENTITIES,
            Task::Relevance => OUT_RELEVANCE,
            Task::Headlines => OUT_HEADLINE,
            _ => 0,
        })
        .sum();
    if per_article_out > 0 {
        let mut overhead = REQUEST_OVERHEAD;
        if config.tasks.contains(&Task::Labels) {
            overhead += label_vocabulary as u64 * TOKENS_PER_LABEL;
        }
        if let Some(c) = &config.relevance_criteria {
            overhead += approx_tokens(c);
        }
        for chunk in scope.needs_processing.chunks(EXTRACT_BATCH) {
            let content: u64 = chunk.iter().map(record_tokens).sum();
            let expected = per_article_out * chunk.len() as u64;
            out.push(PlannedRequest {
                stage: Stage::Extraction,
                overhead_tokens: overhead,
                content_tokens: content,
                expected_output: expected,
                max_output: max_output(expected),
            });
        }
    }

    // Stage 2: validate locally generated candidate pairs only.
    if config.tasks.contains(&Task::Related) {
        for chunk in scope.candidates.chunks(PAIR_BATCH) {
            let content: u64 = chunk.iter().map(|c| record_tokens(&c.a) + record_tokens(&c.b) + 8).sum();
            let expected = OUT_PAIR * chunk.len() as u64;
            out.push(PlannedRequest {
                stage: Stage::RelationshipValidation,
                overhead_tokens: REQUEST_OVERHEAD,
                content_tokens: content,
                expected_output: expected,
                max_output: max_output(expected),
            });
        }
    }

    // Stage 3: hierarchical summary over distinct articles.
    if config.tasks.contains(&Task::Summary) && !scope.records.is_empty() {
        let ids: Vec<i64> = scope.records.iter().map(|r| r.id).collect();
        let chunks: Vec<&[i64]> = ids.chunks(SUMMARY_CHUNK).collect();
        if chunks.len() == 1 {
            out.push(PlannedRequest {
                stage: Stage::FinalSummary,
                overhead_tokens: REQUEST_OVERHEAD,
                content_tokens: chunks[0].iter().map(record_tokens).sum(),
                expected_output: OUT_FINAL_SUMMARY,
                max_output: max_output(OUT_FINAL_SUMMARY),
            });
        } else {
            for chunk in &chunks {
                out.push(PlannedRequest {
                    stage: Stage::ChunkSummaries,
                    overhead_tokens: REQUEST_OVERHEAD,
                    content_tokens: chunk.iter().map(record_tokens).sum(),
                    expected_output: OUT_CHUNK_SUMMARY,
                    max_output: max_output(OUT_CHUNK_SUMMARY),
                });
            }
            out.push(PlannedRequest {
                stage: Stage::FinalSummary,
                overhead_tokens: REQUEST_OVERHEAD,
                content_tokens: chunks.len() as u64 * OUT_CHUNK_SUMMARY,
                expected_output: OUT_FINAL_SUMMARY,
                max_output: max_output(OUT_FINAL_SUMMARY),
            });
        }
    }
    out
}

/// (input multiplier ×100, output multiplier ×100, retry overhead ×100, cache stage overhead)
fn scenario_params(s: Scenario, reasoning: bool) -> (u64, u64, u64, bool) {
    match (s, reasoning) {
        (Scenario::Optimistic, true) => (80, 100, 100, true),
        (Scenario::Expected, true) => (100, 200, 100, false),
        (Scenario::Conservative, true) => (135, 400, 110, false),
        (Scenario::Optimistic, false) => (80, 70, 100, true),
        (Scenario::Expected, false) => (100, 100, 100, false),
        (Scenario::Conservative, false) => (135, 150, 110, false),
    }
}

fn scenario_totals(
    requests: &[PlannedRequest],
    scenario: Scenario,
    price: Option<&Price>,
    batch: bool,
    caching_supported: bool,
) -> ScenarioTotals {
    let reasoning = price.is_none_or(|p| p.reasoning);
    let (in_m, out_m, retry_m, cache) = scenario_params(scenario, reasoning);
    let mut total = Usage::default();
    let mut cost: Option<Micros> = Some(0);
    let mut first_of_stage: Option<Stage> = None;
    for r in requests {
        let prompt = (r.overhead_tokens + r.content_tokens) * in_m / 100;
        let overhead = r.overhead_tokens * in_m / 100;
        // Optimistic: the stable instruction prefix is cached after the first
        // request of each stage.
        let usage = if cache && caching_supported && first_of_stage == Some(r.stage) {
            Usage {
                input: prompt - overhead,
                cached_input: overhead,
                cache_write: 0,
                output: r.expected_output * out_m / 100,
            }
        } else {
            Usage { input: prompt, cached_input: 0, cache_write: 0, output: r.expected_output * out_m / 100 }
        };
        first_of_stage = Some(r.stage);
        total += usage;
        cost = match (cost, price.and_then(|p| p.cost(&usage, batch))) {
            (Some(a), Some(b)) => Some(a + b),
            _ => None,
        };
    }
    let scale = |v: u64| v * retry_m / 100;
    ScenarioTotals {
        usage: Usage {
            input: scale(total.input),
            cached_input: scale(total.cached_input),
            cache_write: scale(total.cache_write),
            output: scale(total.output),
        },
        cost: cost.map(scale),
    }
}

/// Estimates a run. `today` selects dated prices.
pub fn estimate(
    config: &AnalysisConfig,
    scope: &Scope,
    label_vocabulary: usize,
    catalog: &Catalog,
    today: NaiveDate,
) -> Estimate {
    let requests = plan(config, scope, label_vocabulary);
    let price = catalog.price_for(config.provider, &config.model, config.billing, today);
    let price = price.as_ref();
    let caching_supported = price.is_some_and(|p| p.rates.cached_input.is_some());
    // The clickbait check is priced at its own model's rates.
    let (checks, main): (Vec<PlannedRequest>, Vec<PlannedRequest>) =
        requests.iter().partition(|r| r.stage == Stage::ClickbaitCheck);
    let check_price =
        config.clickbait_check().and_then(|c| catalog.price_for(Provider::Openai, &c.model, Billing::ApiKey, today));
    let scenarios: BTreeMap<Scenario, ScenarioTotals> =
        [Scenario::Optimistic, Scenario::Expected, Scenario::Conservative]
            .into_iter()
            // Runs are sent as normal requests and charged at standard prices
            // (the runner never uses provider batch APIs), so never discount here.
            .map(|s| {
                let mut totals = scenario_totals(&main, s, price, false, caching_supported);
                if !checks.is_empty() {
                    let c = scenario_totals(&checks, s, check_price.as_ref(), false, false);
                    totals.usage += c.usage;
                    totals.cost = totals.cost.zip(c.cost).map(|(a, b)| a + b);
                }
                (s, totals)
            })
            .collect();

    let mut stages: Vec<StageTotals> = Vec::new();
    for r in &requests {
        match stages.iter_mut().find(|s| s.stage == r.stage) {
            Some(s) => {
                s.requests += 1;
                s.expected_input += r.overhead_tokens + r.content_tokens;
                s.expected_output += r.expected_output;
            }
            None => stages.push(StageTotals {
                stage: r.stage,
                requests: 1,
                expected_input: r.overhead_tokens + r.content_tokens,
                expected_output: r.expected_output,
            }),
        }
    }
    // Same input inflation and retry allowance as the conservative scenario,
    // with every request using its full output limit.
    let worst_case_tokens =
        requests.iter().map(|r| (r.overhead_tokens + r.content_tokens) * 135 / 100 + r.max_output).sum::<u64>() * 110
            / 100;

    let mut warnings = Vec::new();
    let s = &scope.stats;
    if s.matched > s.selected {
        warnings.push(format!(
            "{} articles match, but the maximum is {}; only the newest {} are included.",
            s.matched, config.max_articles, s.selected
        ));
    }
    if config.tasks.is_empty() {
        warnings.push("No analysis tasks are selected.".into());
    }
    match config.depth {
        Depth::Headlines => {
            if config.tasks.contains(&Task::Summary) || config.tasks.contains(&Task::Entities) {
                warnings
                    .push("Headlines alone give little factual context for summaries and entity extraction.".into());
            }
        }
        Depth::HeadlinesAndDescriptions if s.selected > 0 && s.with_description * 2 < s.selected => {
            warnings.push(format!(
                "Only {} of {} articles have an RSS description; the rest are analyzed from headlines.",
                s.with_description, s.selected
            ))
        }
        Depth::FullText if s.with_full_text < s.selected => warnings.push(format!(
            "Archived text exists for {} of {} articles; the others fall back to their description or headline.",
            s.with_full_text, s.selected
        )),
        _ => {}
    }
    if config.tasks.contains(&Task::Headlines) && config.depth == Depth::Headlines {
        warnings.push(
            "Rewriting headlines needs the article description or text; with headlines only, none are rewritten."
                .into(),
        );
    }
    if let Some(check) = config.clickbait_check()
        && !checks.is_empty()
    {
        warnings.push(format!(
            "A clickbait check ({}, OpenAI Decisions API) runs first; only headlines it rates at least {}% likely \
                clickbait go to {} for a rewrite, so the run usually costs less than this estimate.",
            check.model, check.threshold_percent, config.model
        ));
    }
    if s.truncated > 0 {
        warnings.push(format!("{} long articles are shortened before analysis.", s.truncated));
    }
    if config.use_batch {
        warnings.push("Batch processing is not supported yet; this estimate and the run use standard prices.".into());
    }
    if config.billing == Billing::ChatGptPlan {
        if config.provider == Provider::Openai {
            let paying = if config.check_on_api_credit() {
                "The clickbait check is paid with API credit, and the cost shown is for the check. Everything else \
                    uses your ChatGPT plan allowance."
            } else {
                "Runs use your ChatGPT plan allowance instead of API credit."
            };
            warnings.push(format!(
                "{paying} OpenAI does not publish plan allowances in tokens; the run pauses if the plan or the app's \
                    weekly cap is reached. Output length cannot be capped on the plan, so token use may exceed the \
                    estimate."
            ));
        } else {
            warnings.push(format!("{} cannot be billed to a ChatGPT plan.", config.provider));
        }
    }
    let expected_cost = scenarios[&Scenario::Expected].cost;
    let conservative = &scenarios[&Scenario::Conservative];
    match price {
        None => warnings.push(format!(
            "No price is known for {} on {}. Cost is unknown; you must accept that before a paid run.",
            config.model, config.provider
        )),
        Some(p) if expected_cost.is_none() => {
            warnings.push(format!("Pricing for {} is incomplete; cost is unknown.", p.model))
        }
        _ => {}
    }
    if let (Some(max), Some(c)) = (config.limits.max_cost, conservative.cost)
        && c > max
    {
        let exp = expected_cost.map(format_usd).unwrap_or_else(|| "unknown".into());
        warnings.push(format!(
            "The conservative estimate ({}) exceeds the cost limit ({}); expected {exp}. The run would stop at the limit.",
            format_usd(c),
            format_usd(max)
        ));
    }
    if let Some(max) = config.limits.max_tokens
        && conservative.usage.total() > max
    {
        warnings.push(format!(
            "The conservative estimate ({} tokens) exceeds the token limit ({max}).",
            conservative.usage.total()
        ));
    }
    if let Some(max) = config.limits.max_calls
        && requests.len() as u64 > u64::from(max)
    {
        warnings.push(format!("The plan needs {} requests but the call limit is {max}.", requests.len()));
    }

    Estimate {
        stats: scope.stats.clone(),
        candidate_pairs: scope.candidates.len() as u64,
        stages,
        requests: requests.len() as u64,
        scenarios,
        worst_case_tokens,
        price_known: expected_cost.is_some(),
        price_source: price.map(|p| {
            let verified = p.verified_on.map(|d| format!(", verified {d}")).unwrap_or_default();
            if p.manual { format!("manual price{verified}") } else { format!("{}{verified}", p.source) }
        }),
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_approximation_counts_non_ascii_heavier() {
        assert_eq!(approx_tokens(""), 0);
        assert_eq!(approx_tokens("abcd"), 2);
        let en = approx_tokens("The government proposes changes to unemployment benefits");
        let fi = approx_tokens("Hallitus esittää muutoksia työttömyysturvaan");
        assert!(en > 10 && en < 20, "{en}");
        assert!(fi > 12, "{fi}");
    }
}

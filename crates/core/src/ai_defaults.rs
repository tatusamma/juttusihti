//! Built-in model recommendations and the user's AI defaults, so a quick
//! analysis needs no choices beyond what to do.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::analysis::{AnalysisConfig, ClickbaitCheck, DEFAULT_PARALLEL_REQUESTS, Depth, Task};
use crate::budget::Limits;
use crate::pricing::{Billing, Provider};
use crate::search::SearchQuery;
use crate::store::Store;
use crate::{Result, pricing::Micros};

const SETTING: &str = "ai_defaults";

/// Speed/quality preference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Small, fast, cheap models: right for headlines, labels and relevance.
    #[default]
    Fast,
    /// Larger models: better summaries and subtle story relations.
    Quality,
}

impl Tier {
    pub const ALL: [Tier; 2] = [Tier::Fast, Tier::Quality];

    pub fn label(self) -> &'static str {
        match self {
            Tier::Fast => "Fast (recommended)",
            Tier::Quality => "Quality",
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A recommended model with why it is recommended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recommendation {
    pub provider: Provider,
    pub tier: Tier,
    pub model: &'static str,
    pub note: &'static str,
}

/// Recommendations as of 2026-10-27 (prices in `pricing/catalog.json`).
pub const RECOMMENDED: [Recommendation; 6] = [
    Recommendation {
        provider: Provider::Openai,
        tier: Tier::Fast,
        model: "gpt-6-luna",
        note: "GPT-6 Luna: OpenAI's smallest model, fast and cheapest ($0.10 / $0.50 per million tokens in/out).",
    },
    Recommendation {
        provider: Provider::Openai,
        tier: Tier::Quality,
        model: "gpt-6.1-sol",
        note: "GPT-6.1 Sol: stronger summaries and story relations, about 20× the price of Luna.",
    },
    Recommendation {
        provider: Provider::Anthropic,
        tier: Tier::Fast,
        model: "claude-haiku-5-5",
        note: "Claude Haiku 5.5: fast and cheapest Claude ($0.10 / $0.50 per million tokens in/out).",
    },
    Recommendation {
        provider: Provider::Anthropic,
        tier: Tier::Quality,
        model: "claude-sonnet-5-5",
        note: "Claude Sonnet 5.5: stronger summaries and story relations, about 20× the price of Haiku.",
    },
    Recommendation {
        provider: Provider::Gemini,
        tier: Tier::Fast,
        model: "gemini-3.5-flash-lite",
        note: "Gemini 3.5 Flash-Lite: fast, minimal thinking by default ($0.30 / $2.50 per million tokens in/out).",
    },
    Recommendation {
        provider: Provider::Gemini,
        tier: Tier::Quality,
        model: "gemini-3.8-flash",
        note: "Gemini 3.8 Flash: stronger results; $0.75 / $3.75 per million tokens, doubling on 1 January 2027.",
    },
];

pub fn recommended(provider: Provider, tier: Tier) -> &'static Recommendation {
    RECOMMENDED.iter().find(|r| r.provider == provider && r.tier == tier).expect("every provider has every tier")
}

/// Which tier suits each task, shown next to the task.
pub fn task_hint(task: Task) -> &'static str {
    match task {
        Task::Headlines | Task::Labels | Task::Relevance | Task::Entities => "fast model is enough",
        Task::Related => "fast model is usually enough",
        Task::Summary => "quality model writes better summaries",
    }
}

/// How an OpenAI run pays: the choices offered to the user. Stored as the
/// run's billing plus [`ClickbaitCheck::on_api_credit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayWith {
    ChatGptPlan,
    /// The clickbait check on API credit, everything else on the plan.
    Split,
    ApiKey,
}

impl PayWith {
    pub fn of(billing: Billing, check: &ClickbaitCheck) -> Self {
        match billing {
            Billing::ApiKey => PayWith::ApiKey,
            Billing::ChatGptPlan if check.on_api_credit => PayWith::Split,
            Billing::ChatGptPlan => PayWith::ChatGptPlan,
        }
    }

    pub fn apply(self, billing: &mut Billing, check: &mut ClickbaitCheck) {
        *billing = if self == PayWith::ApiKey { Billing::ApiKey } else { Billing::ChatGptPlan };
        check.on_api_credit = self == PayWith::Split;
        // Paying for the check separately only means something if it runs.
        if self == PayWith::Split {
            check.enabled = true;
        }
    }
}

/// What the user set up once: used by the quick analysis and as the
/// starting point of a custom analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiDefaults {
    pub provider: Provider,
    #[serde(default)]
    pub billing: Billing,
    #[serde(default)]
    pub tier: Tier,
    /// Overrides the tier's recommended model (e.g. a ChatGPT plan model).
    #[serde(default)]
    pub model: Option<String>,
    /// Tasks preselected in the quick analysis.
    pub tasks: BTreeSet<Task>,
    /// Newest articles of the current list to analyze.
    pub max_articles: u32,
    /// Per-run limits.
    pub max_cost: Option<Micros>,
    pub max_tokens: Option<u64>,
    /// Clickbait check before headline rewriting (OpenAI, on API credit).
    #[serde(default)]
    pub clickbait: ClickbaitCheck,
    /// Provider requests in flight at once.
    #[serde(default = "default_parallel_requests")]
    pub parallel_requests: u32,
}

fn default_parallel_requests() -> u32 {
    DEFAULT_PARALLEL_REQUESTS
}

impl Default for AiDefaults {
    fn default() -> Self {
        Self {
            provider: Provider::Openai,
            billing: Billing::ApiKey,
            tier: Tier::Fast,
            model: None,
            tasks: [Task::Headlines, Task::Labels].into_iter().collect(),
            max_articles: 50,
            max_cost: Some(250_000),
            max_tokens: Some(500_000),
            clickbait: ClickbaitCheck::default(),
            parallel_requests: DEFAULT_PARALLEL_REQUESTS,
        }
    }
}

impl AiDefaults {
    /// The model to use: an explicit choice, else the tier's recommendation.
    pub fn model(&self) -> String {
        self.model.clone().unwrap_or_else(|| recommended(self.provider, self.tier).model.to_string())
    }

    /// Only OpenAI can be billed to a ChatGPT plan.
    pub fn billing(&self) -> Billing {
        if self.provider == Provider::Openai { self.billing } else { Billing::ApiKey }
    }

    pub fn pay_with(&self) -> PayWith {
        PayWith::of(self.billing(), &self.clickbait)
    }

    /// Whether runs need an API key for the check on top of the plan sign-in.
    pub fn check_on_api_credit(&self) -> bool {
        self.pay_with() == PayWith::Split && self.clickbait.enabled
    }

    /// A quick analysis of the newest articles matching `scope`, with
    /// headlines and descriptions (the cheapest useful depth) and cache reuse.
    pub fn quick_config(&self, scope: SearchQuery, scope_label: &str, tasks: &BTreeSet<Task>) -> AnalysisConfig {
        AnalysisConfig {
            scope,
            range_label: scope_label.to_string(),
            provider: self.provider,
            billing: self.billing(),
            model: self.model(),
            depth: Depth::HeadlinesAndDescriptions,
            tasks: tasks.clone(),
            limits: Limits {
                max_tokens: self.max_tokens,
                max_cost: self.max_cost,
                max_calls: None,
                // A plan has no per-token price; API keys need a known price.
                allow_unknown_cost: false,
            },
            max_articles: self.max_articles,
            reuse_cached: true,
            use_batch: false,
            relevance_criteria: None,
            clickbait: self.clickbait.clone(),
            parallel_requests: self.parallel_requests,
        }
    }
}

impl Store {
    /// Saved defaults, or built-in ones.
    pub fn ai_defaults(&self) -> Result<AiDefaults> {
        Ok(self.setting(SETTING)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default())
    }

    pub fn set_ai_defaults(&self, d: &AiDefaults) -> Result<()> {
        self.set_setting(SETTING, &serde_json::to_string(d)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::Catalog;

    #[test]
    fn every_recommendation_has_a_known_price() {
        let catalog = Catalog::builtin();
        let day = "2026-10-27".parse().unwrap();
        for r in RECOMMENDED {
            assert!(catalog.lookup(r.provider, r.model, day).is_some(), "{} has no price", r.model);
        }
        for p in Provider::ALL {
            for t in Tier::ALL {
                assert_eq!(recommended(p, t).provider, p);
            }
        }
    }

    #[test]
    fn quick_config_is_cheap_and_bounded() {
        let d = AiDefaults { provider: Provider::Openai, billing: Billing::ChatGptPlan, ..Default::default() };
        let tasks: BTreeSet<Task> = [Task::Headlines].into_iter().collect();
        let c = d.quick_config(SearchQuery { text: "sähkö".into(), ..Default::default() }, "All articles", &tasks);
        assert_eq!(c.model, "gpt-6-luna");
        assert_eq!(c.billing, Billing::ChatGptPlan);
        assert_eq!(c.depth, Depth::HeadlinesAndDescriptions, "no full text in quick runs");
        assert!(c.reuse_cached, "analyzed articles are never paid for twice");
        assert_eq!(c.max_articles, 50);
        assert_eq!(c.limits.max_cost, Some(250_000));
        assert!(!c.limits.allow_unknown_cost);
        assert_eq!(c.scope.text, "sähkö");
    }

    #[test]
    fn pay_with_round_trips_through_billing_and_the_check() {
        for choice in [PayWith::ChatGptPlan, PayWith::Split, PayWith::ApiKey] {
            let mut d = AiDefaults::default();
            choice.apply(&mut d.billing, &mut d.clickbait);
            assert_eq!(d.pay_with(), choice);
            assert_eq!(d.check_on_api_credit(), choice == PayWith::Split);
        }
        // Choosing the split turns a disabled check on.
        let mut d = AiDefaults::default();
        d.clickbait.enabled = false;
        PayWith::Split.apply(&mut d.billing, &mut d.clickbait);
        assert!(d.clickbait.enabled);
        // Unticking the check keeps the split but needs no key.
        d.clickbait.enabled = false;
        assert_eq!(d.pay_with(), PayWith::Split);
        assert!(!d.check_on_api_credit());
        // Other providers have no plan to split.
        let d = AiDefaults { provider: Provider::Gemini, billing: Billing::ChatGptPlan, ..d };
        assert_eq!(d.pay_with(), PayWith::ApiKey);
    }

    #[test]
    fn defaults_round_trip_and_fall_back() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.ai_defaults().unwrap(), AiDefaults::default());
        let mut d = AiDefaults { provider: Provider::Gemini, billing: Billing::ChatGptPlan, ..Default::default() };
        s.set_ai_defaults(&d).unwrap();
        assert_eq!(s.ai_defaults().unwrap(), d);
        assert_eq!(d.billing(), Billing::ApiKey, "only OpenAI bills a ChatGPT plan");
        assert_eq!(d.model(), "gemini-3.5-flash-lite");
        d.model = Some("custom".into());
        assert_eq!(d.model(), "custom");
        s.set_setting("ai_defaults", "{not json").unwrap();
        assert_eq!(s.ai_defaults().unwrap(), AiDefaults::default(), "corrupt settings fall back");
    }
}

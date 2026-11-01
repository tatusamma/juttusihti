//! Updateable model price catalog.
//!
//! Prices are held as integer micro-USD per million tokens so cost
//! arithmetic is exact. A missing price is `None` and stays unknown in every
//! calculation; it is never treated as free.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

const BUILTIN: &str = include_str!("../pricing/catalog.json");

/// Micro-USD (1e-6 USD).
pub type Micros = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Anthropic,
    Openai,
    Gemini,
}

impl Provider {
    pub const ALL: [Provider; 3] = [Provider::Anthropic, Provider::Openai, Provider::Gemini];

    pub fn label(self) -> &'static str {
        match self {
            Provider::Anthropic => "Anthropic Claude",
            Provider::Openai => "OpenAI",
            Provider::Gemini => "Google Gemini",
        }
    }
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// How a run is paid for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Billing {
    /// Per-token API charges on the user's own API key.
    #[default]
    ApiKey,
    /// The user's ChatGPT plan through Sign in with ChatGPT (OpenAI only).
    /// Usage counts against the plan's allowance; there is no per-token
    /// API charge.
    ChatGptPlan,
}

impl Billing {
    pub fn label(self) -> &'static str {
        match self {
            Billing::ApiKey => "API key",
            Billing::ChatGptPlan => "ChatGPT plan",
        }
    }
}

impl std::fmt::Display for Billing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Per-million-token rates for one pricing tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rates {
    pub input: Option<Micros>,
    pub cached_input: Option<Micros>,
    pub cache_write: Option<Micros>,
    pub output: Option<Micros>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LongContext {
    /// Requests whose prompt exceeds this many input tokens use `rates`.
    pub above_input_tokens: u64,
    pub rates: Rates,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Price {
    pub provider: Provider,
    pub model: String,
    pub currency: String,
    pub valid_from: Option<NaiveDate>,
    /// Exclusive.
    pub valid_until: Option<NaiveDate>,
    pub rates: Rates,
    pub long_context: Option<LongContext>,
    /// Multiplier for batch processing in per-mille (500 = 50%), if offered.
    pub batch_permille: Option<u32>,
    /// Whether the model bills reasoning/thinking tokens as output.
    pub reasoning: bool,
    pub source: String,
    pub verified_on: Option<NaiveDate>,
    /// True for user-entered overrides.
    pub manual: bool,
}

impl Price {
    /// Requests charged to a ChatGPT plan have no per-token API cost: the
    /// cost is a known zero, not an unknown price.
    pub fn chatgpt_plan(model: &str) -> Self {
        let zero = Some(0);
        Price {
            provider: Provider::Openai,
            model: model.to_string(),
            currency: "USD".into(),
            valid_from: None,
            valid_until: None,
            rates: Rates { input: zero, cached_input: zero, cache_write: zero, output: zero },
            long_context: None,
            batch_permille: None,
            reasoning: true,
            source: "charged to your ChatGPT plan, no API cost".into(),
            verified_on: None,
            manual: false,
        }
    }

    fn valid_on(&self, day: NaiveDate) -> bool {
        self.valid_from.is_none_or(|f| day >= f) && self.valid_until.is_none_or(|u| day < u)
    }

    /// Rates applying to a request with `prompt_tokens` input tokens.
    pub fn rates_for(&self, prompt_tokens: u64) -> Rates {
        match &self.long_context {
            Some(lc) if prompt_tokens > lc.above_input_tokens => lc.rates,
            _ => self.rates,
        }
    }
}

/// Token usage of one request, split by billing category.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Uncached input tokens.
    pub input: u64,
    pub cached_input: u64,
    pub cache_write: u64,
    /// Visible output plus reasoning tokens (both are billed as output).
    pub output: u64,
}

impl Usage {
    pub fn prompt_tokens(&self) -> u64 {
        self.input + self.cached_input + self.cache_write
    }
    pub fn total(&self) -> u64 {
        self.prompt_tokens() + self.output
    }
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, o: Self) {
        self.input += o.input;
        self.cached_input += o.cached_input;
        self.cache_write += o.cache_write;
        self.output += o.output;
    }
}

/// tokens × micro-USD-per-million, rounded up.
fn charge(tokens: u64, rate: Option<Micros>) -> Option<Micros> {
    if tokens == 0 {
        return Some(0);
    }
    let rate = rate?;
    Some(((tokens as u128 * rate as u128).div_ceil(1_000_000)) as Micros)
}

impl Price {
    /// Cost of one request, or `None` if any used category has no price.
    pub fn cost(&self, usage: &Usage, batch: bool) -> Option<Micros> {
        let r = self.rates_for(usage.prompt_tokens());
        // A cache-write price absent from the catalog falls back to the input
        // price only when the provider has no separate write charge (OpenAI,
        // Gemini implicit caching); Anthropic entries always list it.
        let write_rate = r.cache_write.or(if self.provider == Provider::Anthropic { None } else { r.input });
        let base = charge(usage.input, r.input)?
            + charge(usage.cached_input, r.cached_input)?
            + charge(usage.cache_write, write_rate)?
            + charge(usage.output, r.output)?;
        if batch {
            let m = self.batch_permille?;
            Some((base as u128 * m as u128).div_ceil(1000) as Micros)
        } else {
            Some(base)
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub entries: Vec<Price>,
}

#[derive(Deserialize)]
struct RawCatalog {
    entries: Vec<RawPrice>,
}

#[derive(Deserialize)]
struct RawPrice {
    provider: Provider,
    model: String,
    #[serde(default = "usd")]
    currency: String,
    valid_from: Option<NaiveDate>,
    valid_until: Option<NaiveDate>,
    input: Option<f64>,
    cached_input: Option<f64>,
    cache_write: Option<f64>,
    output: Option<f64>,
    long_context: Option<RawLong>,
    batch_multiplier: Option<f64>,
    #[serde(default)]
    reasoning: bool,
    source: String,
    verified_on: Option<NaiveDate>,
}

#[derive(Deserialize)]
struct RawLong {
    above_input_tokens: u64,
    input: Option<f64>,
    cached_input: Option<f64>,
    cache_write: Option<f64>,
    output: Option<f64>,
}

fn usd() -> String {
    "USD".into()
}

/// USD per million tokens → micro-USD per million tokens.
pub fn usd_to_micros(usd: f64) -> Result<Micros> {
    if !usd.is_finite() || !(0.0..=1_000_000.0).contains(&usd) {
        return Err(Error::Invalid(format!("invalid price {usd}")));
    }
    Ok((usd * 1_000_000.0).round() as Micros)
}

fn rates(input: Option<f64>, cached: Option<f64>, write: Option<f64>, output: Option<f64>) -> Result<Rates> {
    let conv = |v: Option<f64>| v.map(usd_to_micros).transpose();
    Ok(Rates { input: conv(input)?, cached_input: conv(cached)?, cache_write: conv(write)?, output: conv(output)? })
}

impl Catalog {
    pub fn builtin() -> Self {
        Self::from_json(BUILTIN).expect("built-in price catalog is valid")
    }

    pub fn from_json(json: &str) -> Result<Self> {
        let raw: RawCatalog = serde_json::from_str(json)?;
        let entries = raw
            .entries
            .into_iter()
            .map(|p| {
                Ok(Price {
                    rates: rates(p.input, p.cached_input, p.cache_write, p.output)?,
                    long_context: p
                        .long_context
                        .map(|l| {
                            Ok::<_, Error>(LongContext {
                                above_input_tokens: l.above_input_tokens,
                                rates: rates(l.input, l.cached_input, l.cache_write, l.output)?,
                            })
                        })
                        .transpose()?,
                    batch_permille: p
                        .batch_multiplier
                        .map(|m| {
                            if (0.0..=1.0).contains(&m) {
                                Ok((m * 1000.0).round() as u32)
                            } else {
                                Err(Error::Invalid(format!("invalid batch multiplier {m}")))
                            }
                        })
                        .transpose()?,
                    provider: p.provider,
                    model: p.model,
                    currency: p.currency,
                    valid_from: p.valid_from,
                    valid_until: p.valid_until,
                    reasoning: p.reasoning,
                    source: p.source,
                    verified_on: p.verified_on,
                    manual: false,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { entries })
    }

    /// Adds or replaces a manual price. Manual prices take precedence.
    pub fn set_override(&mut self, mut price: Price) {
        price.manual = true;
        self.entries.retain(|p| !(p.manual && p.provider == price.provider && p.model == price.model));
        self.entries.insert(0, price);
    }

    /// The price for a model on a given day. Manual overrides win; otherwise
    /// the catalog entry valid on that day.
    pub fn lookup(&self, provider: Provider, model: &str, day: NaiveDate) -> Option<&Price> {
        let matches = |p: &&Price| p.provider == provider && p.model == model && p.valid_on(day);
        self.entries.iter().filter(matches).find(|p| p.manual).or_else(|| self.entries.iter().find(matches))
    }

    /// Distinct model ids known for a provider.
    /// The price a run with `billing` is charged at.
    pub fn price_for(&self, provider: Provider, model: &str, billing: Billing, day: NaiveDate) -> Option<Price> {
        match billing {
            Billing::ChatGptPlan if provider == Provider::Openai => Some(Price::chatgpt_plan(model)),
            Billing::ChatGptPlan => None,
            Billing::ApiKey => self.lookup(provider, model, day).cloned(),
        }
    }

    pub fn models(&self, provider: Provider) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for p in self.entries.iter().filter(|p| p.provider == provider) {
            if !out.contains(&p.model) {
                out.push(p.model.clone());
            }
        }
        out
    }
}

/// Formats micro-USD as dollars with enough precision for small amounts.
pub fn format_usd(m: Micros) -> String {
    let dollars = m as f64 / 1_000_000.0;
    if dollars >= 1.0 { format!("${dollars:.2}") } else { format!("${dollars:.4}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn builtin_catalog_loads_and_is_exact() {
        let c = Catalog::builtin();
        let p = c.lookup(Provider::Anthropic, "claude-opus-5-5", day("2026-10-25")).unwrap();
        assert_eq!(p.rates.input, Some(4_000_000));
        assert_eq!(p.rates.cached_input, Some(200_000));
        // 1M input + 100k output = $4 + $2.
        let u = Usage { input: 1_000_000, output: 100_000, ..Default::default() };
        assert_eq!(p.cost(&u, false), Some(6_000_000));
        assert_eq!(p.cost(&u, true), Some(3_000_000));
    }

    #[test]
    fn rounds_up_tiny_charges() {
        let p = Catalog::builtin();
        let p = p.lookup(Provider::Anthropic, "claude-haiku-5-5", day("2026-10-25")).unwrap();
        // 1 token at $0.10/MTok = $0.0000001 → rounds up to 1 micro-USD.
        assert_eq!(p.cost(&Usage { input: 1, ..Default::default() }, false), Some(1));
    }

    #[test]
    fn long_context_tier_applies_per_request() {
        let c = Catalog::builtin();
        let haiku = c.lookup(Provider::Anthropic, "claude-haiku-5-5", day("2026-10-25")).unwrap();
        let small = Usage { input: 100_000, ..Default::default() };
        let large = Usage { input: 100_001, ..Default::default() };
        assert_eq!(haiku.cost(&small, false), Some(10_000));
        assert_eq!(haiku.cost(&large, false), Some(50_001));
        // Cached tokens count toward the threshold.
        let mixed = Usage { input: 50_000, cached_input: 60_000, ..Default::default() };
        assert_eq!(haiku.cost(&mixed, false), Some(25_000 + 3_000));
    }

    #[test]
    fn dated_prices_switch_on_their_date() {
        let c = Catalog::builtin();
        let before = c.lookup(Provider::Gemini, "gemini-4-argon", day("2026-11-30")).unwrap();
        let after = c.lookup(Provider::Gemini, "gemini-4-argon", day("2026-12-01")).unwrap();
        assert_eq!(before.rates.input, Some(2_000_000));
        assert_eq!(after.rates.input, Some(4_000_000));
    }

    #[test]
    fn unknown_prices_stay_unknown() {
        let mut c = Catalog::builtin();
        assert!(c.lookup(Provider::Openai, "some-future-model", day("2026-10-25")).is_none());
        // A model without a cache-write price on Anthropic cannot price cache writes.
        let mut p = c.lookup(Provider::Anthropic, "claude-opus-5-5", day("2026-10-25")).unwrap().clone();
        p.rates.cache_write = None;
        assert_eq!(p.cost(&Usage { cache_write: 10, ..Default::default() }, false), None);
        assert_eq!(p.cost(&Usage { input: 10, ..Default::default() }, false), Some(40));
        // Batch requested but not offered → unknown.
        p.batch_permille = None;
        assert_eq!(p.cost(&Usage { input: 10, ..Default::default() }, true), None);
        // Manual override wins.
        let mut manual = p.clone();
        manual.rates.input = Some(1_000_000);
        c.set_override(manual);
        assert_eq!(
            c.lookup(Provider::Anthropic, "claude-opus-5-5", day("2026-10-25")).unwrap().rates.input,
            Some(1_000_000)
        );
    }

    #[test]
    fn rejects_bad_prices() {
        assert!(usd_to_micros(-1.0).is_err());
        assert!(usd_to_micros(f64::NAN).is_err());
        assert!(
            Catalog::from_json(
                r#"{"entries":[{"provider":"openai","model":"x","input":1,"batch_multiplier":2,"source":"s"}]}"#
            )
            .is_err()
        );
    }
}

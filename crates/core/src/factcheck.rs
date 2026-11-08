//! Fact check of one article: the stored result types and the validation
//! that keeps a verdict honest. A verdict needs real evidence: a source the
//! run actually gathered, not an invented one.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::estimate::approx_tokens;
use crate::pricing::{Billing, Micros, Price, Provider, Usage};
use crate::store::Store;
use crate::{ArticleId, Result};

pub const MAX_CLAIMS: usize = 8;
pub const MAX_SOURCES_PER_CLAIM: usize = 5;
pub const FACT_CHECK_PROMPT_VERSION: u32 = 1;

/// Per-check spending limit when the user has not set one: $1.00.
pub const DEFAULT_FACT_CHECK_LIMIT: Micros = 1_000_000;

pub const EXTRACT_MAX_OUTPUT: u32 = 4000;
pub const CLAIM_MAX_OUTPUT: u32 = 1500;
/// Output caps for a model that thinks before answering: its thinking
/// counts against the cap, so the reply needs more room.
pub const EXTRACT_MAX_OUTPUT_REASONING: u32 = 8000;
pub const CLAIM_MAX_OUTPUT_REASONING: u32 = 6000;
pub const SEARCHES_PER_CLAIM: u32 = 3;
pub const SEARCH_TOKENS_EXPECTED: u64 = 4000;
pub const SEARCH_TOKENS_CONSERVATIVE: u64 = 12000;
pub const CLAIM_PROMPT_TOKENS: u64 = 2500;
const EXTRACT_PROMPT_TOKENS: u64 = 1000;

/// Cost range shown before a check is sent. Costs are `None` when the model
/// price or its web search price is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactCheckEstimate {
    pub expected: Option<Micros>,
    pub conservative: Option<Micros>,
    pub searches_expected: u32,
    pub searches_conservative: u32,
    pub tokens_conservative: u64,
}

/// Output token caps for the extraction and for each claim call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputCaps {
    pub extract: u32,
    pub claim: u32,
}

impl OutputCaps {
    /// The bigger caps when the model bills reasoning tokens as output.
    pub fn for_price(price: Option<&Price>) -> Self {
        if price.is_some_and(|p| p.reasoning) {
            OutputCaps { extract: EXTRACT_MAX_OUTPUT_REASONING, claim: CLAIM_MAX_OUTPUT_REASONING }
        } else {
            OutputCaps { extract: EXTRACT_MAX_OUTPUT, claim: CLAIM_MAX_OUTPUT }
        }
    }
}

pub fn estimate(article_text: &str, price: Option<&Price>) -> FactCheckEstimate {
    let caps = OutputCaps::for_price(price);
    let (extract_max, claim_max) = (u64::from(caps.extract), u64::from(caps.claim));
    let claims = MAX_CLAIMS as u32;
    let extract_in = approx_tokens(article_text) + EXTRACT_PROMPT_TOKENS;
    let searches_expected = claims;
    let searches_conservative = claims * SEARCHES_PER_CLAIM;
    // Every claim call resends the whole article.
    let article = approx_tokens(article_text);
    let claim_in_expected = article + claim_in_tokens(1, SEARCH_TOKENS_EXPECTED);
    let claim_in_conservative = article + claim_in_tokens(SEARCHES_PER_CLAIM, SEARCH_TOKENS_CONSERVATIVE);
    let extract_out_expected = extract_max / 2;
    let claim_out_expected = claim_max / 3;
    // Each call is its own request, so long-context rates apply per call.
    let cost = |extract_out: u64, claim_in: u64, claim_out: u64, searches: u32| {
        let p = price?;
        let call = |input: u64, output: u64| p.cost(&Usage { input, output, ..Default::default() }, false);
        Some(call(extract_in, extract_out)? + call(claim_in, claim_out)? * claims as u64 + p.search_cost(searches)?)
    };
    FactCheckEstimate {
        expected: cost(extract_out_expected, claim_in_expected, claim_out_expected, searches_expected),
        conservative: cost(extract_max, claim_in_conservative, claim_max, searches_conservative),
        searches_expected,
        searches_conservative,
        tokens_conservative: extract_in + extract_max + (claim_in_conservative + claim_max) * claims as u64,
    }
}

fn claim_in_tokens(searches: u32, per_search: u64) -> u64 {
    CLAIM_PROMPT_TOKENS + searches as u64 * per_search
}

const LIMIT_SETTING: &str = "fact_check_max_cost";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Supported,
    Contradicted,
    Misleading,
    Unverified,
    NotChecked,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Supported => "Supported",
            Verdict::Contradicted => "Contradicted",
            Verdict::Misleading => "Misleading",
            Verdict::Unverified => "Unverified",
            Verdict::NotChecked => "Not checked (limit reached)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Web,
    FactCheck,
    Wikipedia,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub url: String,
    pub title: String,
    pub kind: SourceKind,
    pub publisher: Option<String>,
    pub rating: Option<String>,
    /// The source's site when its URL does not show it (Gemini's
    /// grounding redirects).
    #[serde(default)]
    pub site: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimResult {
    pub quote: String,
    pub verdict: Verdict,
    pub reason: String,
    pub sources: Vec<Source>,
}

/// The Journalist's Guidelines (JSN) points the check looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Guideline {
    Headline15,
    Sources12,
    Reply21,
    Advertising16,
}

impl Guideline {
    pub fn number(self) -> u8 {
        match self {
            Guideline::Headline15 => 15,
            Guideline::Sources12 => 12,
            Guideline::Reply21 => 21,
            Guideline::Advertising16 => 16,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Guideline::Headline15 => "Headline backed by text",
            Guideline::Sources12 => "Source criticism",
            Guideline::Reply21 => "Right to reply",
            Guideline::Advertising16 => "Hidden advertising",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidelineStatus {
    Ok,
    Concern,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuidelineCheck {
    pub guideline: Guideline,
    pub status: GuidelineStatus,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactCheck {
    pub article_id: ArticleId,
    pub checked_at: i64,
    pub provider: Provider,
    pub model: String,
    pub billing: Billing,
    pub prompt_version: u32,
    pub language: String,
    pub web_search: bool,
    pub claims: Vec<ClaimResult>,
    pub guidelines: Vec<GuidelineCheck>,
    pub cost: Option<Micros>,
    /// Why an evidence lookup could not be used (a rejected Google key),
    /// so it is not mistaken for "no published fact checks".
    #[serde(default)]
    pub lookup_problem: Option<String>,
}

impl FactCheck {
    /// Claims that were contradicted or found misleading.
    pub fn flagged_count(&self) -> u32 {
        self.claims.iter().filter(|c| matches!(c.verdict, Verdict::Contradicted | Verdict::Misleading)).count() as u32
    }

    pub fn summary_line(&self) -> String {
        if self.claims.is_empty() {
            return "No checkable factual claims found.".to_string();
        }
        let count = |v: Verdict| self.claims.iter().filter(|c| c.verdict == v).count();
        // The score leads; problems follow so they are never hidden.
        let mut parts = vec![format!("{} of {} supported", count(Verdict::Supported), self.claims.len())];
        let rest = [
            (Verdict::Contradicted, "contradicted"),
            (Verdict::Misleading, "misleading"),
            (Verdict::Unverified, "unverified"),
            (Verdict::NotChecked, "not checked"),
        ];
        parts.extend(rest.iter().filter_map(|(v, word)| {
            let n = count(*v);
            (n > 0).then(|| format!("{n} {word}"))
        }));
        format!("Fact check: {}", parts.join(" · "))
    }
}

/// A source the model cited for a claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CitedSource {
    pub url: String,
    /// The model says this is a copy of the same story.
    pub same_story: bool,
}

/// Everything this run found: search results, Wikipedia pages and Google
/// Fact Check entries, keyed by normalized URL in the order found. Only
/// these may be cited.
#[derive(Debug, Clone, Default)]
pub struct Gathered {
    sources: Vec<(String, Source)>,
}

impl Gathered {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a source. A URL that is not http or https is ignored; the first
    /// entry for a URL wins.
    pub fn add(&mut self, s: Source) {
        if let Some(key) = normalize_url(&s.url)
            && self.get(&key).is_none()
        {
            self.sources.push((key, s));
        }
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    fn get(&self, key: &str) -> Option<(&str, &Source)> {
        self.sources.iter().find(|(k, _)| k == key).map(|(k, s)| (k.as_str(), s))
    }

    /// The first source whose site is `host` (without "www.").
    fn on_site(&self, host: &str) -> Option<(&str, &Source)> {
        self.sources
            .iter()
            .find(|(_, s)| s.site.as_deref().map(bare_host).as_deref() == Some(host))
            .map(|(k, s)| (k.as_str(), s))
    }
}

/// Lowercase host, no fragment, no trailing slash. `None` unless http(s).
fn normalize_url(raw: &str) -> Option<String> {
    let mut url = Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.host_str()?;
    url.set_fragment(None);
    let mut out = url.to_string();
    if url.query().is_none() && out.ends_with('/') {
        out.pop();
    }
    Some(out)
}

fn host_of(raw: &str) -> Option<String> {
    Url::parse(raw.trim()).ok()?.host_str().map(bare_host)
}

/// Lowercase, without a leading "www.".
fn bare_host(h: &str) -> String {
    let h = h.trim().to_ascii_lowercase();
    h.strip_prefix("www.").map(str::to_string).unwrap_or(h)
}

/// Applies the evidence rules to one claim: keeps only cited sources that
/// were gathered, are not on the article's own domain and are not copies of
/// the same story, at most [`MAX_SOURCES_PER_CLAIM`]. A decided verdict
/// without a non-Wikipedia source becomes `Unverified`.
///
/// A source's site, when known, stands in for its URL's host: Gemini's
/// sources are redirect URLs, so a cited page that was not gathered as such
/// resolves to the first gathered source on its site.
pub fn judge_sources(
    verdict: Verdict,
    cited: &[CitedSource],
    gathered: &Gathered,
    article_url: Option<&str>,
) -> (Verdict, Vec<Source>) {
    let own = article_url.and_then(host_of);
    let mut seen: Vec<&str> = Vec::new();
    let mut kept: Vec<Source> = Vec::new();
    for c in cited {
        if c.same_story || kept.len() >= MAX_SOURCES_PER_CLAIM {
            continue;
        }
        let Some(key) = normalize_url(&c.url) else {
            continue;
        };
        let found = gathered.get(&key).or_else(|| gathered.on_site(&host_of(&c.url)?));
        let Some((key, source)) = found else {
            continue;
        };
        if seen.contains(&key) {
            continue;
        }
        let site = source.site.as_deref().map(bare_host).or_else(|| host_of(&source.url));
        if let (Some(own), Some(site)) = (&own, site)
            && (site == *own || site.ends_with(&format!(".{own}")))
        {
            continue;
        }
        seen.push(key);
        kept.push(source.clone());
    }
    let decided = matches!(verdict, Verdict::Supported | Verdict::Contradicted | Verdict::Misleading);
    let has_evidence = kept.iter().any(|s| s.kind != SourceKind::Wikipedia);
    if decided && !has_evidence {
        return (Verdict::Unverified, kept);
    }
    (verdict, kept)
}

/// Trims, collapses whitespace and cuts to `max_chars` characters, adding
/// "…" when something was cut.
pub fn bound_text(s: &str, max_chars: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let mut out: String = collapsed.chars().take(max_chars).collect();
    out.push('…');
    out
}

impl Store {
    /// Stores the check as the article's only one, replacing any earlier.
    pub fn save_fact_check(&self, fc: &FactCheck) -> Result<()> {
        let provider = serde_json::to_value(fc.provider)?;
        self.conn().execute(
            "INSERT INTO article_fact_checks(article_id, checked_at, provider, model, prompt_version, flagged_count, result_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(article_id) DO UPDATE SET
               checked_at = excluded.checked_at, provider = excluded.provider, model = excluded.model,
               prompt_version = excluded.prompt_version, flagged_count = excluded.flagged_count,
               result_json = excluded.result_json",
            params![
                fc.article_id,
                fc.checked_at,
                provider.as_str().unwrap_or_default(),
                fc.model,
                fc.prompt_version,
                fc.flagged_count(),
                serde_json::to_string(fc)?,
            ],
        )?;
        Ok(())
    }

    /// The article's latest check. A row that no longer parses (written by a
    /// different version) reads as no check.
    pub fn fact_check(&self, article: ArticleId) -> Result<Option<FactCheck>> {
        let json: Option<String> = self
            .conn()
            .query_row("SELECT result_json FROM article_fact_checks WHERE article_id = ?1", [article], |r| r.get(0))
            .optional()?;
        Ok(json.and_then(|j| serde_json::from_str(&j).ok()))
    }

    /// Which of `articles` have a check with contradicted or misleading claims.
    pub fn fact_check_flags(&self, articles: &[ArticleId]) -> Result<HashSet<ArticleId>> {
        let mut flagged = HashSet::new();
        // Stay under SQLite's bound-variable limit.
        for chunk in articles.chunks(500) {
            let marks = vec!["?"; chunk.len()].join(",");
            let mut stmt = self.conn().prepare(&format!(
                "SELECT article_id FROM article_fact_checks WHERE flagged_count > 0 AND article_id IN ({marks})"
            ))?;
            for id in stmt.query_map(params_from_iter(chunk), |r| r.get::<_, ArticleId>(0))? {
                flagged.insert(id?);
            }
        }
        Ok(flagged)
    }

    pub fn fact_check_limit(&self) -> Result<Micros> {
        Ok(self.setting(LIMIT_SETTING)?.and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_FACT_CHECK_LIMIT))
    }

    pub fn set_fact_check_limit(&self, m: Micros) -> Result<()> {
        self.set_setting(LIMIT_SETTING, &m.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::Catalog;

    fn priced(search: Option<f64>) -> Price {
        let json = format!(
            r#"{{"entries":[{{"provider":"anthropic","model":"m","input":2,"output":10,{} "source":"s"}}]}}"#,
            search.map(|s| format!(r#""web_search_per_1k":{s},"#)).unwrap_or_default()
        );
        Catalog::from_json(&json).unwrap().entries.remove(0)
    }

    #[test]
    fn estimate_conservative_exceeds_expected() {
        let p = priced(Some(10.0));
        let e = estimate("Some article text. ".repeat(200).as_str(), Some(&p));
        assert!(e.conservative.unwrap() > e.expected.unwrap());
        assert_eq!(e.searches_expected, 8);
        assert_eq!(e.searches_conservative, 24);
        assert!(e.tokens_conservative > 0);
    }

    #[test]
    fn estimate_exact_figures() {
        let json = r#"{"entries":[{"provider":"anthropic","model":"m","input":1,"output":2,"web_search_per_1k":10,"source":"s"}]}"#;
        let p = Catalog::from_json(json).unwrap().entries.remove(0);
        let e = estimate("", Some(&p));
        assert_eq!(e.tokens_conservative, 325_000);
        assert_eq!((e.searches_expected, e.searches_conservative), (8, 24));
        assert_eq!(e.conservative, Some(581_000));
        assert_eq!(e.expected, Some(145_000));
    }

    #[test]
    fn estimate_prices_each_call_separately() {
        let json = r#"{"entries":[{"provider":"anthropic","model":"m","input":1,"output":2,"web_search_per_1k":10,
            "long_context":{"above_input_tokens":100000,"input":5,"output":10},"source":"s"}]}"#;
        let p = Catalog::from_json(json).unwrap().entries.remove(0);
        let e = estimate("", Some(&p));
        let call = |input, output| p.cost(&Usage { input, output, ..Default::default() }, false).unwrap();
        // Every call is far below the long-context threshold, so base rates apply.
        let expected = call(1_000, 4_000) + 8 * call(38_500, 1_500) + p.search_cost(24).unwrap();
        assert_eq!(e.conservative, Some(expected));
        assert_eq!(e.conservative, Some(581_000));
    }

    #[test]
    fn estimate_with_reasoning_price_uses_bigger_caps() {
        let json = r#"{"entries":[{"provider":"anthropic","model":"m","input":1,"output":2,"web_search_per_1k":10,
            "reasoning":true,"source":"s"}]}"#;
        let p = Catalog::from_json(json).unwrap().entries.remove(0);
        let e = estimate("", Some(&p));
        let call = |input, output| p.cost(&Usage { input, output, ..Default::default() }, false).unwrap();
        // Extraction may write 8000 tokens and each claim 6000, thinking included.
        assert_eq!(e.conservative, Some(call(1_000, 8_000) + 8 * call(38_500, 6_000) + p.search_cost(24).unwrap()));
        assert_eq!(e.conservative, Some(661_000));
        assert_eq!(e.expected, Some(call(1_000, 4_000) + 8 * call(6_500, 2_000) + p.search_cost(8).unwrap()));
        assert_eq!(e.expected, Some(173_000));
        assert_eq!(e.tokens_conservative, 365_000);
    }

    #[test]
    fn estimate_counts_the_article_in_every_claim_call() {
        let json = r#"{"entries":[{"provider":"anthropic","model":"m","input":1,"output":2,"web_search_per_1k":10,"source":"s"}]}"#;
        let p = Catalog::from_json(json).unwrap().entries.remove(0);
        // 3800 ASCII characters are 1000 tokens.
        let text = "x".repeat(3_800);
        assert_eq!(approx_tokens(&text), 1_000);
        let e = estimate(&text, Some(&p));
        // Extraction 2000 in; each claim call resends the article: 39 500 in at most, 7 500 expected.
        assert_eq!(e.conservative, Some(10_000 + 8 * 42_500 + 240_000));
        assert_eq!(e.expected, Some(6_000 + 8 * 8_500 + 80_000));
        assert_eq!(e.tokens_conservative, 2_000 + 4_000 + 8 * (39_500 + 1_500));
    }

    #[test]
    fn estimate_is_unknown_without_search_price() {
        let e = estimate("text", Some(&priced(None)));
        assert_eq!((e.expected, e.conservative), (None, None));
        let e = estimate("text", None);
        assert_eq!((e.expected, e.conservative), (None, None));
    }

    fn src(url: &str, kind: SourceKind) -> Source {
        Source { url: url.into(), title: "t".into(), kind, publisher: None, rating: None, site: None }
    }

    fn cite(url: &str) -> CitedSource {
        CitedSource { url: url.into(), same_story: false }
    }

    fn claim(verdict: Verdict) -> ClaimResult {
        ClaimResult { quote: "q".into(), verdict, reason: "r".into(), sources: vec![] }
    }

    fn check(verdicts: &[Verdict]) -> FactCheck {
        FactCheck {
            article_id: 1,
            checked_at: 0,
            provider: Provider::Anthropic,
            model: "m".into(),
            billing: Billing::default(),
            prompt_version: FACT_CHECK_PROMPT_VERSION,
            language: "en".into(),
            web_search: true,
            claims: verdicts.iter().map(|v| claim(*v)).collect(),
            guidelines: vec![],
            cost: None,
            lookup_problem: None,
        }
    }

    #[test]
    fn a_check_saved_before_lookup_problems_still_loads() {
        let mut v = serde_json::to_value(check(&[Verdict::Supported])).unwrap();
        v.as_object_mut().unwrap().remove("lookup_problem");
        let fc: FactCheck = serde_json::from_value(v).unwrap();
        assert_eq!(fc.lookup_problem, None);
    }

    #[test]
    fn cited_urls_must_come_from_gathered_results() {
        let mut g = Gathered::new();
        g.add(src("https://a.example/x", SourceKind::Web));
        let cited = [cite("https://a.example/x"), cite("https://invented.example/y")];
        let (v, s) = judge_sources(Verdict::Contradicted, &cited, &g, None);
        assert_eq!(v, Verdict::Contradicted);
        assert_eq!(s, vec![src("https://a.example/x", SourceKind::Web)]);
    }

    #[test]
    fn invented_only_sources_downgrade_to_unverified() {
        let g = Gathered::new();
        let (v, s) = judge_sources(Verdict::Supported, &[cite("https://invented.example/y")], &g, None);
        assert_eq!(v, Verdict::Unverified);
        assert!(s.is_empty());
    }

    #[test]
    fn own_domain_and_same_story_sources_are_dropped() {
        let mut g = Gathered::new();
        for u in [
            "https://news.example/b",
            "https://sub.news.example/c",
            "https://other.example/d",
            "https://third.example/e",
        ] {
            g.add(src(u, SourceKind::Web));
        }
        let cited = [
            cite("https://news.example/b"),
            cite("https://sub.news.example/c"),
            CitedSource { url: "https://other.example/d".into(), same_story: true },
            cite("https://third.example/e"),
        ];
        let (v, s) = judge_sources(Verdict::Supported, &cited, &g, Some("https://www.news.example/a/1"));
        assert_eq!(v, Verdict::Supported);
        assert_eq!(s, vec![src("https://third.example/e", SourceKind::Web)]);
    }

    const REDIRECT: &str = "https://vertexaisearch.cloud.google.com/grounding-api-redirect/";

    fn on_site(path: &str, site: &str) -> Source {
        Source { site: Some(site.into()), ..src(&format!("{REDIRECT}{path}"), SourceKind::Web) }
    }

    #[test]
    fn own_domain_check_uses_the_site_of_a_redirect() {
        let mut g = Gathered::new();
        for (path, site) in [("own", "news.example"), ("sub", "www.sub.news.example"), ("other", "other.example")] {
            g.add(on_site(path, site));
        }
        let cited: Vec<CitedSource> = ["own", "sub", "other"].iter().map(|p| cite(&format!("{REDIRECT}{p}"))).collect();
        let (v, s) = judge_sources(Verdict::Supported, &cited, &g, Some("https://www.news.example/a/1"));
        assert_eq!(v, Verdict::Supported);
        assert_eq!(s, vec![on_site("other", "other.example")]);
    }

    #[test]
    fn cited_page_resolves_to_the_redirect_of_its_site() {
        let mut g = Gathered::new();
        g.add(on_site("1", "other.example"));
        g.add(on_site("2", "Other.example"));
        g.add(src("https://plain.example/x", SourceKind::Web));
        let cited = [
            cite("https://www.other.example/report"),
            cite("https://other.example/second"),
            cite("https://unknown.example/y"),
            cite("https://plain.example/elsewhere"),
        ];
        let (v, s) = judge_sources(Verdict::Contradicted, &cited, &g, None);
        assert_eq!(v, Verdict::Contradicted);
        // The first redirect of the site, once; a source without a site is matched by its URL only.
        assert_eq!(s, vec![on_site("1", "other.example")]);
        let (_, s) = judge_sources(Verdict::Contradicted, &cited[..1], &g, Some("https://other.example/a"));
        assert!(s.is_empty(), "the resolved source is still on the article's own site");
    }

    #[test]
    fn stored_sources_without_a_site_still_read() {
        let json = r#"{"url":"https://a.example/x","title":"t","kind":"web","publisher":null,"rating":null}"#;
        let s: Source = serde_json::from_str(json).unwrap();
        assert_eq!(s.site, None);
    }

    #[test]
    fn wikipedia_alone_cannot_decide() {
        let mut g = Gathered::new();
        g.add(src("https://fi.wikipedia.org/wiki/X", SourceKind::Wikipedia));
        let (v, s) = judge_sources(Verdict::Supported, &[cite("https://fi.wikipedia.org/wiki/X")], &g, None);
        assert_eq!(v, Verdict::Unverified);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].kind, SourceKind::Wikipedia);
    }

    #[test]
    fn fact_check_source_counts_as_evidence() {
        let mut g = Gathered::new();
        g.add(src("https://fc.example/1", SourceKind::FactCheck));
        let (v, s) = judge_sources(Verdict::Contradicted, &[cite("https://fc.example/1")], &g, None);
        assert_eq!(v, Verdict::Contradicted);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn url_normalization_ignores_fragment_case_and_trailing_slash() {
        let mut g = Gathered::new();
        g.add(src("https://A.example/x/#frag", SourceKind::Web));
        let (v, s) = judge_sources(Verdict::Supported, &[cite("https://a.example/x")], &g, None);
        assert_eq!(v, Verdict::Supported);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn non_http_urls_are_rejected() {
        let mut g = Gathered::new();
        g.add(src("javascript:alert(1)", SourceKind::Web));
        assert_eq!(g.len(), 0);
        let (v, s) = judge_sources(Verdict::Supported, &[cite("javascript:alert(1)")], &g, None);
        assert_eq!(v, Verdict::Unverified);
        assert!(s.is_empty());
    }

    #[test]
    fn at_most_five_sources_are_kept() {
        let mut g = Gathered::new();
        let urls: Vec<String> = (0..8).map(|i| format!("https://s{i}.example/p")).collect();
        for u in &urls {
            g.add(src(u, SourceKind::Web));
        }
        let cited: Vec<CitedSource> = urls.iter().map(|u| cite(u)).collect();
        let (_, s) = judge_sources(Verdict::Supported, &cited, &g, None);
        assert_eq!(s.len(), MAX_SOURCES_PER_CLAIM);
    }

    #[test]
    fn summary_line_counts_verdicts() {
        let c = check(&[Verdict::Contradicted, Verdict::Unverified, Verdict::Supported, Verdict::Supported]);
        assert_eq!(c.summary_line(), "Fact check: 2 of 4 supported · 1 contradicted · 1 unverified");
        let none = check(&[Verdict::Unverified, Verdict::Unverified]);
        assert_eq!(none.summary_line(), "Fact check: 0 of 2 supported · 2 unverified");
        assert_eq!(check(&[Verdict::Supported]).summary_line(), "Fact check: 1 of 1 supported");
        assert_eq!(c.flagged_count(), 1);
        assert_eq!(check(&[]).summary_line(), "No checkable factual claims found.");
    }

    #[test]
    fn bound_text_cuts_on_char_boundary() {
        assert_eq!(bound_text("ääää  ä", 3), "äää…");
        assert_eq!(bound_text("  a   b ", 10), "a b");
    }
}

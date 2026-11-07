//! Fact check of one article: the stored result types and the validation
//! that keeps a verdict honest. A verdict needs real evidence: a source the
//! run actually gathered, not an invented one.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::ArticleId;
use crate::pricing::{Billing, Micros, Provider};

pub const MAX_CLAIMS: usize = 8;
pub const MAX_SOURCES_PER_CLAIM: usize = 5;
pub const FACT_CHECK_PROMPT_VERSION: u32 = 1;

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
        let order = [
            (Verdict::Contradicted, "contradicted"),
            (Verdict::Misleading, "misleading"),
            (Verdict::Unverified, "unverified"),
            (Verdict::Supported, "supported"),
            (Verdict::NotChecked, "not checked"),
        ];
        let parts: Vec<String> = order
            .iter()
            .filter_map(|(verdict, word)| {
                let n = self.claims.iter().filter(|c| c.verdict == *verdict).count();
                (n > 0).then(|| format!("{n} {word}"))
            })
            .collect();
        format!("Fact check: {}", parts.join(", "))
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
/// Fact Check entries, keyed by normalized URL. Only these may be cited.
#[derive(Debug, Clone, Default)]
pub struct Gathered {
    by_url: HashMap<String, Source>,
}

impl Gathered {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a source. A URL that is not http or https is ignored; the first
    /// entry for a URL wins.
    pub fn add(&mut self, s: Source) {
        if let Some(key) = normalize_url(&s.url) {
            self.by_url.entry(key).or_insert(s);
        }
    }

    pub fn len(&self) -> usize {
        self.by_url.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_url.is_empty()
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
    Url::parse(raw.trim()).ok()?.host_str().map(|h| h.to_ascii_lowercase())
}

/// Applies the evidence rules to one claim: keeps only cited sources that
/// were gathered, are not on the article's own domain and are not copies of
/// the same story, at most [`MAX_SOURCES_PER_CLAIM`]. A decided verdict
/// without a non-Wikipedia source becomes `Unverified`.
pub fn judge_sources(
    verdict: Verdict,
    cited: &[CitedSource],
    gathered: &Gathered,
    article_url: Option<&str>,
) -> (Verdict, Vec<Source>) {
    let own = article_url.and_then(host_of).map(|h| h.strip_prefix("www.").map(str::to_string).unwrap_or(h));
    let mut seen: Vec<String> = Vec::new();
    let mut kept: Vec<Source> = Vec::new();
    for c in cited {
        if c.same_story || kept.len() >= MAX_SOURCES_PER_CLAIM {
            continue;
        }
        let Some(key) = normalize_url(&c.url) else {
            continue;
        };
        let Some(source) = gathered.by_url.get(&key) else {
            continue;
        };
        if seen.contains(&key) {
            continue;
        }
        if let (Some(own), Some(host)) = (&own, host_of(&source.url))
            && (host == *own || host.ends_with(&format!(".{own}")))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn src(url: &str, kind: SourceKind) -> Source {
        Source { url: url.into(), title: "t".into(), kind, publisher: None, rating: None }
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
        }
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
        assert_eq!(c.summary_line(), "Fact check: 1 contradicted, 1 unverified, 2 supported");
        assert_eq!(c.flagged_count(), 1);
        assert_eq!(check(&[]).summary_line(), "No checkable factual claims found.");
    }

    #[test]
    fn bound_text_cuts_on_char_boundary() {
        assert_eq!(bound_text("ääää  ä", 3), "äää…");
        assert_eq!(bound_text("  a   b ", 10), "a b");
    }
}

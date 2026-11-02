//! User-initiated analysis: configuration, scope preview, local
//! deduplication, cache reuse and local candidate-pair generation.
//!
//! Everything here runs locally. Nothing is sent to an AI provider; the
//! output feeds the estimator and, after explicit approval, a session.

use std::collections::{BTreeSet, HashMap};

use rusqlite::types::Value;
use rusqlite::{OptionalExtension, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::budget;
use crate::normalize::{normalize_text, sha256_hex};
use crate::pricing::{Billing, Provider};
use crate::search::{SearchQuery, build_where};
use crate::store::Store;
use crate::{ArticleId, Result};

/// Bumped whenever a task's prompt changes, so cached results are not reused
/// across incompatible prompts.
pub const PROMPT_VERSION: &str = "2026-10-25.1";

/// Bumped whenever the clickbait check's criteria change, so cached
/// probabilities are not reused across incompatible criteria.
pub const CLICKBAIT_PROMPT_VERSION: &str = "2026-11-01.1";

/// Provider requests in flight at once, unless a run sets its own.
pub const DEFAULT_PARALLEL_REQUESTS: u32 = 4;

fn default_parallel_requests() -> u32 {
    DEFAULT_PARALLEL_REQUESTS
}

/// Per-article input caps, so a single long article cannot dominate a run.
pub const MAX_DESCRIPTION_CHARS: usize = 1_500;
pub const MAX_FULLTEXT_CHARS: usize = 12_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Depth {
    /// Fastest and cheapest; little factual context.
    Headlines,
    /// Recommended default for labeling, clustering and relevance.
    #[default]
    HeadlinesAndDescriptions,
    /// Only text that was actually archived locally; never fetched for analysis.
    FullText,
}

impl Depth {
    pub const ALL: [Depth; 3] = [Depth::Headlines, Depth::HeadlinesAndDescriptions, Depth::FullText];

    pub fn label(self) -> &'static str {
        match self {
            Depth::Headlines => "Headlines only",
            Depth::HeadlinesAndDescriptions => "Headlines + RSS descriptions",
            Depth::FullText => "Archived article text",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Depth::Headlines => "h",
            Depth::HeadlinesAndDescriptions => "hd",
            Depth::FullText => "ft",
        }
    }
}

impl std::fmt::Display for Depth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Task {
    Labels,
    Related,
    Entities,
    Relevance,
    Summary,
    /// Informative replacements for clickbait or vague headlines.
    Headlines,
}

impl Task {
    pub const ALL: [Task; 6] =
        [Task::Labels, Task::Related, Task::Entities, Task::Relevance, Task::Headlines, Task::Summary];

    pub fn label(self) -> &'static str {
        match self {
            Task::Labels => "Suggest labels",
            Task::Related => "Find related stories",
            Task::Entities => "Extract entities & locations",
            Task::Relevance => "Relevance filtering",
            Task::Summary => "Generate research summary",
            Task::Headlines => "Rewrite clickbait headlines",
        }
    }

    /// Tasks whose result is per article and therefore cacheable per article.
    pub fn per_article(self) -> bool {
        matches!(self, Task::Labels | Task::Entities | Task::Relevance | Task::Headlines)
    }

    fn key(self) -> &'static str {
        match self {
            Task::Labels => "labels",
            Task::Related => "related",
            Task::Entities => "entities",
            Task::Relevance => "relevance",
            Task::Summary => "summary",
            Task::Headlines => "headlines",
        }
    }
}

/// Everything the user chose on the New Analysis screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisConfig {
    /// Scope. `from`/`to` are UTC, inclusive start and exclusive end.
    pub scope: SearchQuery,
    /// What the user entered, e.g. "1.10.2026 – 25.10.2026 (Europe/Helsinki)".
    pub range_label: String,
    pub provider: Provider,
    /// API key or ChatGPT plan; older sessions are API-key runs.
    #[serde(default)]
    pub billing: Billing,
    pub model: String,
    pub depth: Depth,
    pub tasks: BTreeSet<Task>,
    pub limits: budget::Limits,
    pub max_articles: u32,
    pub reuse_cached: bool,
    /// Provider batch APIs are not supported yet: runs always use normal
    /// requests at standard prices. Kept so sessions saved with the flag
    /// still load; it has no effect on cost.
    #[serde(default)]
    pub use_batch: bool,
    /// Investigation topic for relevance filtering.
    pub relevance_criteria: Option<String>,
    /// Clickbait check before headline rewriting. Sessions saved before it
    /// existed were approved without it and run without it.
    #[serde(default = "ClickbaitCheck::off")]
    pub clickbait: ClickbaitCheck,
    /// Provider requests in flight at once. Each one's worst case is
    /// reserved in the budget before it is sent, so limits still hold.
    #[serde(default = "default_parallel_requests")]
    pub parallel_requests: u32,
}

impl AnalysisConfig {
    /// The clickbait check, if it runs for this configuration. It needs the
    /// headline task with article text (a rewrite must be grounded in it)
    /// and an OpenAI API key: the Decisions API is OpenAI's and is not
    /// documented for ChatGPT plans, so plan runs keep the one-stage rewrite
    /// unless the user chose to pay for the check with API credit.
    pub fn clickbait_check(&self) -> Option<&ClickbaitCheck> {
        (self.clickbait.enabled
            && self.tasks.contains(&Task::Headlines)
            && self.depth != Depth::Headlines
            && self.provider == Provider::Openai
            && (self.billing == Billing::ApiKey || self.clickbait.on_api_credit))
            .then_some(&self.clickbait)
    }

    /// Whether this run's check is paid with API credit while the rest of
    /// the run is on a ChatGPT plan, so it needs both credentials.
    pub fn check_on_api_credit(&self) -> bool {
        self.billing == Billing::ChatGptPlan && self.clickbait_check().is_some()
    }

    /// Requests in flight at once (at least one).
    pub fn parallel_requests(&self) -> usize {
        self.parallel_requests.clamp(1, 32) as usize
    }
}

/// A cheap first stage for headline rewriting: OpenAI's Decisions API
/// estimates for each headline the probability that it is clickbait, and
/// only headlines at or above the threshold go to the rewriting model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClickbaitCheck {
    pub enabled: bool,
    /// Decisions API model.
    pub model: String,
    /// Clickbait probability, in percent, from which a headline is rewritten.
    pub threshold_percent: u8,
    /// Headlines classified per request.
    pub batch_size: u32,
    /// Seconds to wait for one classification request.
    pub timeout_secs: u32,
    /// On a ChatGPT plan run, pay for the check with API credit.
    pub on_api_credit: bool,
}

impl Default for ClickbaitCheck {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "gpt-6-luna".into(),
            threshold_percent: 70,
            batch_size: 50,
            timeout_secs: 30,
            on_api_credit: false,
        }
    }
}

impl ClickbaitCheck {
    /// Questions the Decisions API takes in one request.
    pub const MAX_BATCH: u32 = 400;

    pub fn off() -> Self {
        Self { enabled: false, ..Default::default() }
    }

    /// The threshold as a probability, 0.70 by default.
    pub fn threshold(&self) -> f64 {
        f64::from(self.threshold_percent.min(100)) / 100.0
    }

    pub fn is_clickbait(&self, probability: f64) -> bool {
        probability >= self.threshold()
    }

    pub fn batch_size(&self) -> usize {
        self.batch_size.clamp(1, Self::MAX_BATCH) as usize
    }
}

/// Compact per-article input for the AI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticleRecord {
    pub id: ArticleId,
    pub feed_id: i64,
    pub effective_at: i64,
    pub title: String,
    /// Description or archived text, depending on depth (possibly empty).
    pub body: String,
    pub truncated: bool,
    pub content_hash: String,
    pub dedup_key: String,
}

impl ArticleRecord {
    /// The text sent for this article: one compact line plus body.
    pub fn prompt_text(&self) -> String {
        if self.body.is_empty() {
            format!("[{}] {}", self.id, self.title)
        } else {
            format!("[{}] {}\n{}", self.id, self.title, self.body)
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeStats {
    /// Articles matching the filters and date range.
    pub matched: u64,
    /// Matched articles taken into this run (bounded by `max_articles`).
    pub selected: u64,
    /// Distinct stories after collapsing duplicates/syndicated copies.
    pub distinct: u64,
    /// Distinct articles whose per-article tasks are all cached.
    pub cached: u64,
    /// Distinct articles needing per-article AI work.
    pub to_process: u64,
    /// Selected articles with a non-empty description / archived text.
    pub with_description: u64,
    pub with_full_text: u64,
    pub truncated: u64,
    /// Duplicate pairs found locally (no AI needed).
    pub local_duplicates: u64,
}

/// The locally prepared scope for an analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct Scope {
    pub stats: ScopeStats,
    /// Distinct articles (one per dedup key), newest first.
    pub records: Vec<ArticleRecord>,
    /// Ids of `records` that still need per-article AI work.
    pub needs_processing: Vec<ArticleId>,
    /// Every selected id, including duplicates (the session snapshot).
    pub selected_ids: Vec<ArticleId>,
    pub candidates: Vec<Candidate>,
}

/// A locally generated pair worth asking the AI about.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Candidate {
    pub a: ArticleId,
    pub b: ArticleId,
    /// Shared significant title words.
    pub shared_terms: u32,
}

/// One row of the session list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: i64,
    pub created_at: i64,
    pub status: crate::SessionStatus,
    pub range_label: String,
    pub model: String,
}

/// Cache key for one article and task.
pub fn cache_key(content_hash: &str, provider: Provider, model: &str, task: Task, depth: Depth) -> String {
    let provider = serde_json::to_string(&provider).unwrap_or_default();
    sha256_hex(&format!("{content_hash}|{provider}|{model}|{}|{PROMPT_VERSION}|{}", task.key(), depth.key()))
}

/// Cache key for one article's clickbait probability. The threshold is not
/// part of it: a new threshold reuses the probabilities already paid for.
pub fn clickbait_key(content_hash: &str, check: &ClickbaitCheck, depth: Depth) -> String {
    sha256_hex(&format!("{content_hash}|clickbait|{}|{CLICKBAIT_PROMPT_VERSION}|{}", check.model, depth.key()))
}

fn cut(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        return (s.to_string(), false);
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    (out, true)
}

/// Words too common to signal that two headlines cover the same story.
const STOPWORDS: &[&str] = &[
    // English
    "about",
    "after",
    "again",
    "against",
    "also",
    "been",
    "being",
    "from",
    "have",
    "into",
    "more",
    "news",
    "over",
    "says",
    "said",
    "than",
    "that",
    "their",
    "there",
    "these",
    "they",
    "this",
    "what",
    "when",
    "where",
    "which",
    "while",
    "with",
    "will",
    "would",
    "your",
    "year",
    "years",
    "today",
    // Finnish
    "ettei",
    "jotka",
    "joka",
    "jonka",
    "koska",
    "kanssa",
    "mutta",
    "myös",
    "nyt",
    "olla",
    "ovat",
    "sekä",
    "sitä",
    "tämä",
    "tänään",
    "vielä",
    "voi",
    "vuoden",
    "vuonna",
    "kertoo",
    "mukaan",
    "uutiset",
    "jälkeen",
    "ennen",
];

fn significant_terms(title: &str) -> BTreeSet<String> {
    normalize_text(title)
        .split(' ')
        .filter(|w| w.chars().count() >= 4 && !STOPWORDS.contains(w) && !w.chars().all(|c| c.is_ascii_digit()))
        // Light prefix stemming so Finnish inflections ("hallitus"/"hallituksen") meet.
        .map(|w| w.chars().take(6).collect())
        .collect()
}

/// Generates candidate pairs locally: distinct articles within `window_secs`
/// of each other sharing at least two significant headline terms. Each
/// article keeps at most `per_article` strongest candidates, and the total is
/// capped. This replaces any all-pairs LLM comparison.
pub fn candidate_pairs(records: &[ArticleRecord], window_secs: i64, per_article: usize, cap: usize) -> Vec<Candidate> {
    let terms: Vec<BTreeSet<String>> = records.iter().map(|r| significant_terms(&r.title)).collect();
    let mut index: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, t) in terms.iter().enumerate() {
        for w in t {
            index.entry(w.as_str()).or_default().push(i);
        }
    }
    let mut shared: HashMap<(usize, usize), u32> = HashMap::new();
    for postings in index.values() {
        // Very common terms carry little signal and would explode pair counts.
        if postings.len() > 200 {
            continue;
        }
        for (x, &i) in postings.iter().enumerate() {
            for &j in &postings[x + 1..] {
                if (records[i].effective_at - records[j].effective_at).abs() <= window_secs {
                    *shared.entry((i.min(j), i.max(j))).or_default() += 1;
                }
            }
        }
    }
    let mut pairs: Vec<((usize, usize), u32)> = shared.into_iter().filter(|(_, n)| *n >= 2).collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut per: HashMap<usize, usize> = HashMap::new();
    let mut out = Vec::new();
    for ((i, j), n) in pairs {
        if out.len() >= cap {
            break;
        }
        if per.get(&i).copied().unwrap_or(0) >= per_article || per.get(&j).copied().unwrap_or(0) >= per_article {
            continue;
        }
        *per.entry(i).or_default() += 1;
        *per.entry(j).or_default() += 1;
        out.push(Candidate { a: records[i].id, b: records[j].id, shared_terms: n });
    }
    out
}

impl Store {
    /// Builds the local scope for a configuration: matching, dedup, cache
    /// lookup and candidate generation. Read-only.
    pub fn analysis_scope(&self, config: &AnalysisConfig) -> Result<Scope> {
        let (from_where, params, _) = build_where(&config.scope)?;
        self.scope_from(config, &from_where, params)
    }

    /// Rebuilds the scope of a saved session from its article snapshot, so a
    /// run (or resume) covers exactly the articles that were approved.
    pub fn session_scope(&self, session_id: i64, config: &AnalysisConfig) -> Result<Scope> {
        let from_where =
            " FROM articles a JOIN analysis_session_articles s ON s.article_id = a.id WHERE s.session_id = ?";
        self.scope_from(config, from_where, vec![Value::Integer(session_id)])
    }

    fn scope_from(&self, config: &AnalysisConfig, from_where: &str, mut params: Vec<Value>) -> Result<Scope> {
        let conn = self.conn();
        let matched: i64 =
            conn.query_row(&format!("SELECT count(*){from_where}"), params_from_iter(params.iter()), |r| r.get(0))?;

        params.push(Value::Integer(i64::from(config.max_articles)));
        // Text fetched from the article page (full-article feeds) replaces a
        // teaser as full-text input. `from_where` always starts with
        // " FROM articles a", so the extract join slots in right after it.
        debug_assert!(from_where.starts_with(" FROM articles a "));
        let from_where_extract = from_where.replacen(
            " FROM articles a ",
            " FROM articles a LEFT JOIN article_extracts ext ON ext.article_id = a.id AND ext.status = 'ok' ",
            1,
        );
        let sql = format!(
            "SELECT a.id, a.feed_id, a.effective_at, a.title, COALESCE(a.summary_text, ''), COALESCE(a.content_text, ''),
                    a.content_hash, a.dedup_key, ext.text, ext.text_hash
             {from_where_extract} ORDER BY a.effective_at DESC, a.id DESC LIMIT ?"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params.iter()), |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
            ))
        })?;

        let mut stats = ScopeStats { matched: matched as u64, ..Default::default() };
        let mut selected_ids = Vec::new();
        let mut records: Vec<ArticleRecord> = Vec::new();
        let mut seen: HashMap<String, ArticleId> = HashMap::new();
        for row in rows {
            let (
                id,
                feed_id,
                effective_at,
                title,
                summary,
                content,
                mut content_hash,
                dedup_key,
                extract,
                extract_hash,
            ) = row?;
            if let (Some(h), Depth::FullText) = (&extract_hash, config.depth) {
                // Results cached for the teaser must not be reused for the full text.
                content_hash = crate::normalize::sha256_hex(&format!("{content_hash}|extract|{h}"));
            }
            let content = extract.unwrap_or(content);
            selected_ids.push(id);
            stats.with_description += u64::from(!summary.trim().is_empty());
            stats.with_full_text += u64::from(!content.trim().is_empty());
            if seen.contains_key(&dedup_key) {
                stats.local_duplicates += 1;
                continue;
            }
            seen.insert(dedup_key.clone(), id);
            let (body, truncated) = match config.depth {
                Depth::Headlines => (String::new(), false),
                Depth::HeadlinesAndDescriptions => cut(summary.trim(), MAX_DESCRIPTION_CHARS),
                Depth::FullText => {
                    let src = if content.trim().is_empty() { summary.trim() } else { content.trim() };
                    cut(src, MAX_FULLTEXT_CHARS)
                }
            };
            stats.truncated += u64::from(truncated);
            records.push(ArticleRecord { id, feed_id, effective_at, title, body, truncated, content_hash, dedup_key });
        }
        stats.selected = selected_ids.len() as u64;
        stats.distinct = records.len() as u64;

        let mut needs_processing = Vec::new();
        if config.tasks.iter().any(|t| t.per_article()) {
            for r in &records {
                if self.open_tasks(config, r, config.reuse_cached)?.is_empty() {
                    stats.cached += 1;
                } else {
                    needs_processing.push(r.id);
                }
            }
        }
        stats.to_process = needs_processing.len() as u64;

        let candidates = if config.tasks.contains(&Task::Related) {
            candidate_pairs(&records, 7 * 86_400, 5, 5_000)
        } else {
            Vec::new()
        };
        Ok(Scope { stats, records, needs_processing, selected_ids, candidates })
    }

    /// The per-article tasks still to do for one article. With `reuse`, a
    /// task with a cached result is done. Under the clickbait check, the
    /// headline task is also done when no rewrite could be grounded (no
    /// article text) or a cached probability is below the threshold; an
    /// unclassified headline stays open, to be classified first.
    pub fn open_tasks(&self, config: &AnalysisConfig, r: &ArticleRecord, reuse: bool) -> Result<BTreeSet<Task>> {
        let mut open = BTreeSet::new();
        let mut cached = self.conn().prepare_cached("SELECT 1 FROM ai_result_cache WHERE cache_key = ?1")?;
        for t in config.tasks.iter().copied().filter(|t| t.per_article()) {
            let key = cache_key(&r.content_hash, config.provider, &config.model, t, config.depth);
            if !(reuse && cached.exists([key])?) {
                open.insert(t);
            }
        }
        if let Some(check) = config.clickbait_check()
            && open.contains(&Task::Headlines)
        {
            let settled = r.body.trim().is_empty()
                || (reuse
                    && self
                        .cached_clickbait(&clickbait_key(&r.content_hash, check, config.depth))?
                        .is_some_and(|p| !check.is_clickbait(p)));
            if settled {
                open.remove(&Task::Headlines);
            }
        }
        Ok(open)
    }

    /// A cached clickbait probability.
    pub fn cached_clickbait(&self, key: &str) -> Result<Option<f64>> {
        let json: Option<String> = self
            .conn()
            .prepare_cached("SELECT result_json FROM ai_result_cache WHERE cache_key = ?1")?
            .query_row([key], |r| r.get(0))
            .optional()?;
        Ok(json
            .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
            .and_then(|v| v["probability"].as_f64())
            .filter(|p| (0.0..=1.0).contains(p)))
    }

    /// Stores a per-article AI result for reuse by later runs.
    pub fn cache_result(&self, key: &str, article: ArticleId, task: Task, result: &serde_json::Value) -> Result<()> {
        self.conn().execute(
            "INSERT INTO ai_result_cache(cache_key, article_id, task, result_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(cache_key) DO UPDATE SET result_json = excluded.result_json",
            rusqlite::params![key, article, task.key(), serde_json::to_string(result)?, crate::now()],
        )?;
        Ok(())
    }

    /// Records an estimated session with its article snapshot and moves it
    /// to `awaiting_approval`. Running still needs an explicit approval step.
    pub fn save_estimated_session(
        &mut self,
        config: &AnalysisConfig,
        scope: &Scope,
        estimate: &crate::estimate::Estimate,
    ) -> Result<i64> {
        let id = self.create_session(&serde_json::to_value(config)?, &scope.selected_ids)?;
        self.set_session_estimate(id, &serde_json::to_value(estimate)?)?;
        self.set_session_status(id, crate::SessionStatus::AwaitingApproval, None)?;
        Ok(id)
    }

    /// Recent sessions, newest first.
    pub fn recent_sessions(&self, limit: u32) -> Result<Vec<SessionSummary>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, created_at, status,
                    COALESCE(json_extract(config_json, '$.range_label'), ''),
                    COALESCE(json_extract(config_json, '$.model'), '')
             FROM analysis_sessions ORDER BY created_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |r| {
            Ok(SessionSummary {
                id: r.get(0)?,
                created_at: r.get(1)?,
                status: r.get(2)?,
                range_label: r.get(3)?,
                model: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn is_cached(&self, key: &str) -> Result<bool> {
        Ok(self.conn().prepare_cached("SELECT 1 FROM ai_result_cache WHERE cache_key = ?1")?.exists([key])?)
    }

    /// Usage already charged to a session, so a resumed run keeps counting
    /// against the same budget.
    pub fn session_usage(&self, session: i64) -> Result<budget::Totals> {
        let (tokens, cost, unknown, calls): (i64, i64, i64, i64) = self.conn().query_row(
            "SELECT COALESCE(sum(input_tokens + cached_input_tokens + output_tokens + reasoning_tokens), 0),
                    COALESCE(sum(cost_microunits), 0), count(*) - count(cost_microunits), count(*)
             FROM usage_ledger WHERE session_id = ?1",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        Ok(budget::Totals { tokens: tokens as u64, cost: cost as u64, cost_unknown: unknown > 0, calls: calls as u32 })
    }

    /// Appends one provider call to the usage ledger. `cost` is `None` when
    /// unpriced; it is stored as NULL, never as zero.
    #[allow(clippy::too_many_arguments)]
    pub fn record_usage(
        &self,
        session: Option<i64>,
        provider: Provider,
        model: &str,
        operation: &str,
        usage: &crate::pricing::Usage,
        reasoning_tokens: u64,
        cost: Option<crate::pricing::Micros>,
    ) -> Result<()> {
        let provider = serde_json::to_value(provider)?;
        // `usage.output` already includes reasoning tokens; the ledger keeps
        // them apart so reports can show both.
        let visible_output = usage.output.saturating_sub(reasoning_tokens);
        self.conn().execute(
            "INSERT INTO usage_ledger(session_id, provider, model, operation, input_tokens, cached_input_tokens,
                                      output_tokens, reasoning_tokens, cost_microunits, currency, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'USD', ?10)",
            rusqlite::params![
                session,
                provider.as_str().unwrap_or_default(),
                model,
                operation,
                (usage.input + usage.cache_write) as i64,
                usage.cached_input as i64,
                visible_output as i64,
                reasoning_tokens as i64,
                cost.map(|c| c as i64),
                crate::now()
            ],
        )?;
        Ok(())
    }

    /// Usage recorded since `since` (UTC seconds), for daily/monthly budgets.
    pub fn usage_since(&self, since: i64) -> Result<budget::Totals> {
        let (tokens, cost, unknown, calls): (i64, i64, i64, i64) = self.conn().query_row(
            "SELECT COALESCE(sum(input_tokens + cached_input_tokens + output_tokens + reasoning_tokens), 0),
                    COALESCE(sum(cost_microunits), 0),
                    count(*) - count(cost_microunits),
                    count(*)
             FROM usage_ledger WHERE created_at >= ?1",
            [since],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        Ok(budget::Totals { tokens: tokens as u64, cost: cost as u64, cost_unknown: unknown > 0, calls: calls as u32 })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: i64, title: &str, at: i64) -> ArticleRecord {
        ArticleRecord {
            id,
            feed_id: 1,
            effective_at: at,
            title: title.into(),
            body: String::new(),
            truncated: false,
            content_hash: String::new(),
            dedup_key: id.to_string(),
        }
    }

    #[test]
    fn candidates_need_shared_terms_and_proximity() {
        let day = 86_400;
        let records = vec![
            rec(1, "Hallitus esittää muutoksia työttömyysturvaan", 0),
            rec(2, "Hallituksen esitys työttömyysturvasta herättää kritiikkiä", day),
            rec(3, "Lumisade jatkuu Lapissa", day),
            rec(4, "Hallitus esittää muutoksia työttömyysturvaan", 30 * day),
            rec(5, "City council approves tram line", 0),
            rec(6, "Tram line approved by council", day),
        ];
        let pairs = candidate_pairs(&records, 7 * day, 5, 100);
        let ids: Vec<(i64, i64)> = pairs.iter().map(|c| (c.a, c.b)).collect();
        assert!(ids.contains(&(1, 2)), "Finnish inflections meet via prefix stems: {ids:?}");
        assert!(ids.contains(&(5, 6)));
        assert!(!ids.iter().any(|p| p.0 == 4 || p.1 == 4), "outside the time window");
        assert!(!ids.iter().any(|p| p.0 == 3 || p.1 == 3));
    }

    #[test]
    fn candidates_are_capped() {
        let records: Vec<_> = (0..50).map(|i| rec(i, "Storm floods coastal towns overnight", 0)).collect();
        let pairs = candidate_pairs(&records, 86_400, 2, 1_000);
        assert!(pairs.len() <= 50, "per-article cap bounds pairs, got {}", pairs.len());
        assert_eq!(candidate_pairs(&records, 86_400, 50, 10).len(), 10);
    }

    #[test]
    fn cache_key_depends_on_every_input() {
        let base = cache_key("h", Provider::Anthropic, "m", Task::Labels, Depth::Headlines);
        assert_ne!(base, cache_key("h2", Provider::Anthropic, "m", Task::Labels, Depth::Headlines));
        assert_ne!(base, cache_key("h", Provider::Openai, "m", Task::Labels, Depth::Headlines));
        assert_ne!(base, cache_key("h", Provider::Anthropic, "m2", Task::Labels, Depth::Headlines));
        assert_ne!(base, cache_key("h", Provider::Anthropic, "m", Task::Entities, Depth::Headlines));
        assert_ne!(base, cache_key("h", Provider::Anthropic, "m", Task::Labels, Depth::FullText));
    }
}

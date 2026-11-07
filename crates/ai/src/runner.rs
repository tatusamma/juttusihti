//! Executes an approved analysis session.
//!
//! Order of operations for every provider call:
//! 1. check pause/cancel,
//! 2. reserve the call's worst case with the budget guard (refusal pauses
//!    the session with the reason),
//! 3. send; on cancel while in flight, stop waiting and charge the worst
//!    case because the provider may still bill an accepted request,
//! 4. settle the budget and append the usage ledger with actual usage,
//! 5. validate output and persist suggestions with provenance,
//! 6. cache per-article results so a resume never repeats paid work.
//!
//! Headline rewriting has two stages when the configuration allows it
//! ([`AnalysisConfig::clickbait_check`]): the Decisions API first rates each
//! headline's clickbait probability in large batches, and only headlines at
//! or above the threshold are sent to the rewriting model. A failed check
//! never drops an article: its headline goes to the rewriting model as it
//! would without the check.
//!
//! Source articles are never modified.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use juttusihti_core::analysis::{
    AnalysisConfig, ArticleRecord, ClickbaitCheck, PROMPT_VERSION, Scope, Task, cache_key as article_cache_key,
    clickbait_key,
};
use juttusihti_core::budget::{Guard, Refusal, Reservation};
use juttusihti_core::normalize::sha256_hex;
use juttusihti_core::pricing::{Billing, Catalog, Micros, Price, Usage};
use juttusihti_core::research::{NewLocation, NewSuggestion};
use juttusihti_core::store::Store;
use juttusihti_core::{EntityKind, Origin, SessionStatus};
use tokio::sync::{Notify, mpsc};

use crate::prompts::{self, Summary};
use crate::{AiError, Client, Outcome, Reply, Request, decisions};

const EXTRACT_BATCH: usize = 20;
const PAIR_BATCH: usize = 10;
const SUMMARY_CHUNK: usize = 40;
/// Retries of a batch whose output failed validation or was truncated.
const OUTPUT_RETRIES: u32 = 1;
/// Output tokens reserved per clickbait question (they are not charged).
const CHECK_OUTPUT_PER_QUESTION: u64 = 64;
const CHECK_OPERATION: &str = "clickbait_check";
/// Reasoning effort for per-article extraction and pair checks: simple,
/// well-specified tasks that need little thinking. Summaries keep the
/// model's default.
const LIGHT_EFFORT: &str = "low";

/// Pause/cancel switch shared with the UI.
#[derive(Clone, Default)]
pub struct Control(Arc<(AtomicU8, Notify)>);

const RUNNING: u8 = 0;
const PAUSE: u8 = 1;
const CANCEL: u8 = 2;

impl Control {
    pub fn pause(&self) {
        let _ = self.0.0.compare_exchange(RUNNING, PAUSE, Ordering::SeqCst, Ordering::SeqCst);
    }
    pub fn cancel(&self) {
        self.0.0.store(CANCEL, Ordering::SeqCst);
        self.0.1.notify_waiters();
    }
    pub fn reset(&self) {
        self.0.0.store(RUNNING, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.state() == CANCEL
    }
    fn state(&self) -> u8 {
        self.0.0.load(Ordering::SeqCst)
    }
    pub(crate) async fn cancelled(&self) {
        loop {
            let notified = self.0.1.notified();
            if self.state() == CANCEL {
                return;
            }
            notified.await;
        }
    }
}

/// The user's explicit approval for a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Approval {
    /// The user agreed to send the selected article content to the provider.
    pub consent_to_send: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Progress {
    Stage { name: &'static str, requests: usize },
    Call { stage: &'static str, done: usize, total: usize, tokens: u64, cost: Option<Micros> },
    Note(String),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunSummary {
    pub calls: u32,
    pub usage: Usage,
    pub cost: Option<Micros>,
    pub suggestions: u32,
    pub final_status: Option<SessionStatus>,
    /// Why a run paused or failed.
    pub stop_reason: Option<String>,
    pub headlines: HeadlineStats,
    /// Wall time of the run.
    pub elapsed_ms: u64,
}

/// What the clickbait check did in a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeadlineStats {
    /// Headlines that needed a decision.
    pub considered: u32,
    /// Rated by the Decisions API in this run.
    pub classified: u32,
    /// Rated earlier; the cached probability was reused.
    pub cached: u32,
    /// At or above the threshold: sent to the rewriting model.
    pub clickbait: u32,
    /// Below the threshold: kept as they are, no rewrite requested.
    pub rewrites_avoided: u32,
    /// Not rated (error, refusal, budget): sent to the rewriting model, as without the check.
    pub fallbacks: u32,
    pub check_requests: u32,
    pub check_errors: u32,
    /// Wall time of the clickbait check and of the extraction (rewriting) stage.
    pub check_ms: u64,
    pub extraction_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("the run needs explicit consent to send article content to the provider")]
    NoConsent,
    #[error("session cannot run from status {0:?}")]
    NotRunnable(SessionStatus),
    #[error("{0}")]
    Store(#[from] juttusihti_core::Error),
    #[error("stored session is invalid: {0}")]
    BadSession(String),
    #[error("provider {provider:?} does not match session provider {session:?}")]
    ProviderMismatch { provider: juttusihti_core::pricing::Provider, session: juttusihti_core::pricing::Provider },
    #[error("the session is billed to {session}, but the client uses {client}")]
    BillingMismatch { client: juttusihti_core::pricing::Billing, session: juttusihti_core::pricing::Billing },
    #[error("the clickbait check is paid with API credit, but the run has no OpenAI API key for it")]
    NoCheckClient,
}

/// A sent request's budget reservation, settled when it returns.
struct InFlight {
    reservation: Reservation,
    worst_usage: Usage,
    worst_cost: Option<Micros>,
}

/// A run stopped by an empty OpenAI API account; it resumes after a top-up.
fn no_credit() -> Stop {
    Stop::Paused(
        "Your OpenAI API account has no credit left. Add credit at \
            https://platform.openai.com/settings/organization/billing/, then resume."
            .into(),
    )
}

enum Stop {
    Paused(String),
    Cancelled,
    Failed(String),
}

pub struct Runner {
    db: Arc<Mutex<Store>>,
    client: Client,
    /// Sends the clickbait check when it is paid differently from the run.
    check_client: Option<Client>,
    catalog: Catalog,
    control: Control,
    progress: Option<mpsc::UnboundedSender<Progress>>,
}

struct Ctx {
    session: i64,
    config: AnalysisConfig,
    price: Option<Price>,
    /// Price of the clickbait check's model.
    check_price: Option<Price>,
    guard: Guard,
    summary: RunSummary,
}

impl Runner {
    pub fn new(db: Arc<Mutex<Store>>, client: Client, catalog: Catalog, control: Control) -> Self {
        Self { db, client, check_client: None, catalog, control, progress: None }
    }

    /// The API-key client for a plan run's clickbait check.
    pub fn with_check_client(mut self, client: Client) -> Self {
        self.check_client = Some(client);
        self
    }

    pub fn with_progress(mut self, tx: mpsc::UnboundedSender<Progress>) -> Self {
        self.progress = Some(tx);
        self
    }

    fn db<T>(&self, f: impl FnOnce(&mut Store) -> juttusihti_core::Result<T>) -> juttusihti_core::Result<T> {
        let mut s = self.db.lock().map_err(|_| juttusihti_core::Error::Invalid("database lock poisoned".into()))?;
        f(&mut s)
    }

    fn emit(&self, p: Progress) {
        if let Some(tx) = &self.progress {
            let _ = tx.send(p);
        }
    }

    /// Runs (or resumes) a session. Returns when it completes, pauses,
    /// fails or is cancelled; the final status is in the summary.
    pub async fn run(&self, session: i64, approval: Approval) -> Result<RunSummary, RunError> {
        if !approval.consent_to_send {
            return Err(RunError::NoConsent);
        }
        let rec = self.db(|s| s.session(session))?;
        if !matches!(rec.status, SessionStatus::AwaitingApproval | SessionStatus::Paused | SessionStatus::Failed) {
            return Err(RunError::NotRunnable(rec.status));
        }
        let config: AnalysisConfig =
            serde_json::from_value(rec.config).map_err(|e| RunError::BadSession(e.to_string()))?;
        if config.provider != self.client.provider() {
            return Err(RunError::ProviderMismatch { provider: self.client.provider(), session: config.provider });
        }
        // The estimate was approved for one way of paying; never switch it.
        if config.billing != self.client.billing() {
            return Err(RunError::BillingMismatch { client: self.client.billing(), session: config.billing });
        }
        if config.check_on_api_credit() && !self.check_client.as_ref().is_some_and(|c| c.billing() == Billing::ApiKey) {
            return Err(RunError::NoCheckClient);
        }
        let started = std::time::Instant::now();
        let today = Utc::now().date_naive();
        let price = self.catalog.price_for(config.provider, &config.model, config.billing, today);
        let check_price = config
            .clickbait_check()
            .and_then(|c| self.catalog.price_for(config.provider, &c.model, Billing::ApiKey, today));
        let spent = self.db(|s| s.session_usage(session))?;
        let guard = Guard::new(config.limits, spent);
        self.control.reset();
        self.db(|s| s.set_session_status(session, SessionStatus::Running, None))?;

        let mut ctx = Ctx {
            session,
            config,
            price,
            check_price,
            guard,
            summary: RunSummary { cost: Some(0), ..Default::default() },
        };
        let scope = self.db(|s| s.session_scope(session, &ctx.config))?;
        let result = self.execute(&mut ctx, &scope).await;
        let (status, error) = match result {
            Ok(()) => (SessionStatus::Completed, None),
            Err(Stop::Paused(reason)) => (SessionStatus::Paused, Some(reason)),
            Err(Stop::Cancelled) => (SessionStatus::Cancelled, None),
            Err(Stop::Failed(reason)) => (SessionStatus::Failed, Some(reason)),
        };
        if let Some(e) = &error {
            self.emit(Progress::Note(e.clone()));
        }
        self.db(|s| s.set_session_status(session, status, error.as_deref()))?;
        ctx.summary.final_status = Some(status);
        ctx.summary.stop_reason = error;
        ctx.summary.elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(ctx.summary)
    }

    async fn execute(&self, ctx: &mut Ctx, scope: &Scope) -> Result<(), Stop> {
        let tasks = ctx.config.tasks.clone();
        let per_article: BTreeSet<Task> = tasks.iter().copied().filter(|t| t.per_article()).collect();
        if !per_article.is_empty() {
            self.extraction(ctx, scope, &per_article).await?;
        }
        if tasks.contains(&Task::Related) {
            self.related(ctx, scope).await?;
        }
        if tasks.contains(&Task::Summary) {
            self.summarize(ctx, scope).await?;
        }
        Ok(())
    }

    fn check_control(&self) -> Result<(), Stop> {
        match self.control.state() {
            PAUSE => Err(Stop::Paused("paused by user".into())),
            CANCEL => Err(Stop::Cancelled),
            _ => Ok(()),
        }
    }

    /// One budgeted provider call with ledger accounting. Returns parsed
    /// JSON, or `None` when the output was unusable (refused/truncated/not
    /// JSON); usage is charged either way.
    async fn call(
        &self,
        ctx: &mut Ctx,
        operation: &'static str,
        req: &Request,
    ) -> Result<Option<serde_json::Value>, Stop> {
        let flight = self.begin(ctx, req)?;
        let result = self.send(req).await;
        self.finish(ctx, operation, flight, result)
    }

    /// Checks pause/cancel and reserves the request's worst case.
    fn begin(&self, ctx: &mut Ctx, req: &Request) -> Result<InFlight, Stop> {
        self.check_control()?;
        let prompt_estimate = juttusihti_core::estimate::approx_tokens(&req.system)
            + juttusihti_core::estimate::approx_tokens(&req.user)
            + juttusihti_core::estimate::approx_tokens(&req.schema.to_string());
        let worst_usage = Usage {
            input: prompt_estimate * 135 / 100 + 64,
            output: u64::from(req.max_output_tokens),
            ..Default::default()
        };
        let worst_cost = ctx.price.as_ref().and_then(|p| p.cost(&worst_usage, false));
        let reservation = ctx.guard.reserve(worst_usage.total(), worst_cost).map_err(|r| match r {
            Refusal::Paused => Stop::Paused("paused".into()),
            other => Stop::Paused(format!("stopped before the next request: {other}")),
        })?;
        Ok(InFlight { reservation, worst_usage, worst_cost })
    }

    /// Sends a request; `None` if the run was cancelled while waiting.
    async fn send(&self, req: &Request) -> Option<Result<Reply, AiError>> {
        tokio::select! {
            r = self.client.complete(req) => Some(r),
            _ = self.control.cancelled() => None,
        }
    }

    /// Settles a returned (or abandoned) request in the budget and ledger.
    fn finish(
        &self,
        ctx: &mut Ctx,
        operation: &'static str,
        flight: InFlight,
        result: Option<Result<Reply, AiError>>,
    ) -> Result<Option<serde_json::Value>, Stop> {
        let InFlight { reservation, worst_usage, worst_cost } = flight;
        let Some(result) = result else {
            // Cancelled in flight: the provider may still bill the request.
            ctx.guard.settle(reservation, None);
            self.ledger(ctx, &format!("{operation}:cancelled_in_flight"), &worst_usage, 0, worst_cost);
            self.emit(Progress::Note(
                "Cancelled during a request; it may still be charged by the provider (counted at its maximum).".into(),
            ));
            return Err(Stop::Cancelled);
        };
        match result {
            Ok(reply) => {
                let cost = ctx.price.as_ref().and_then(|p| p.cost(&reply.usage, false));
                ctx.guard.settle(reservation, Some((reply.usage.total(), cost)));
                self.ledger(ctx, operation, &reply.usage, reply.reasoning_tokens, cost);
                match reply.outcome {
                    Outcome::Json(v) => Ok(Some(v)),
                    Outcome::Refused(why) => {
                        self.emit(Progress::Note(format!("The provider declined one {operation} request: {why}")));
                        Ok(None)
                    }
                    Outcome::Truncated => {
                        self.emit(Progress::Note(format!("A {operation} response hit its output limit.")));
                        Ok(None)
                    }
                    Outcome::NotJson(text) => {
                        let shown: String = text.chars().take(160).collect();
                        tracing::warn!(operation, text = %text, "model output was not JSON");
                        self.emit(Progress::Note(if shown.trim().is_empty() {
                            format!("A {operation} response was empty.")
                        } else {
                            format!("A {operation} response was not valid JSON: {shown}")
                        }));
                        Ok(None)
                    }
                }
            }
            Err(e) => {
                if e.possibly_charged() {
                    ctx.guard.settle(reservation, None);
                    self.ledger(ctx, &format!("{operation}:unknown_outcome"), &worst_usage, 0, worst_cost);
                } else {
                    ctx.guard.release(reservation);
                }
                match e {
                    // Resumable later; the plan resets on OpenAI's schedule.
                    AiError::NoCredit(_) => Err(no_credit()),
                    AiError::PlanLimit(m) => Err(Stop::Paused(format!(
                        "ChatGPT plan usage limit reached ({m}). Check ChatGPT settings → Usage, then resume."
                    ))),
                    AiError::Status { status: 401 | 403, .. } if ctx.config.billing == Billing::ChatGptPlan => {
                        Err(Stop::Failed("OpenAI rejected the ChatGPT sign-in; sign in again under AI setup.".into()))
                    }
                    AiError::Status { status: 401 | 403, .. } => {
                        Err(Stop::Failed("The provider rejected the API key.".into()))
                    }
                    other => Err(Stop::Failed(other.to_string())),
                }
            }
        }
    }

    fn ledger(&self, ctx: &mut Ctx, operation: &str, usage: &Usage, reasoning: u64, cost: Option<Micros>) {
        let model = ctx.config.model.clone();
        self.ledger_for(ctx, &model, operation, usage, reasoning, cost);
    }

    fn ledger_for(
        &self,
        ctx: &mut Ctx,
        model: &str,
        operation: &str,
        usage: &Usage,
        reasoning: u64,
        cost: Option<Micros>,
    ) {
        ctx.summary.calls += 1;
        ctx.summary.usage += *usage;
        ctx.summary.cost = match (ctx.summary.cost, cost) {
            (Some(a), Some(b)) => Some(a + b),
            _ => None,
        };
        let (session, provider) = (ctx.session, ctx.config.provider);
        if let Err(e) = self.db(|s| s.record_usage(Some(session), provider, model, operation, usage, reasoning, cost)) {
            tracing::error!(error = %e, "could not write usage ledger");
        }
    }

    fn suggestion(
        &self,
        ctx: &Ctx,
        kind: &str,
        payload: serde_json::Value,
        confidence: Option<f64>,
        evidence: Option<String>,
    ) -> NewSuggestion {
        NewSuggestion {
            session_id: Some(ctx.session),
            kind: kind.into(),
            payload,
            provider: serde_json::to_value(ctx.config.provider)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            model: ctx.config.model.clone(),
            prompt_version: PROMPT_VERSION.into(),
            confidence,
            evidence,
        }
    }

    // ---- stage 1: labels / entities / relevance ----------------------------------

    async fn extraction(&self, ctx: &mut Ctx, scope: &Scope, tasks: &BTreeSet<Task>) -> Result<(), Stop> {
        // Re-check the cache: a resumed run skips work finished earlier. Each
        // article gets only its open tasks.
        let config = ctx.config.clone();
        let mut open: Vec<(&ArticleRecord, BTreeSet<Task>)> = scope
            .records
            .iter()
            .filter(|r| scope.needs_processing.contains(&r.id))
            .map(|r| (r, self.db(|s| s.open_tasks(&config, r, true)).unwrap_or_else(|_| tasks.clone())))
            .filter(|(_, t)| !t.is_empty())
            .collect();
        if let Some(check) = config.clickbait_check() {
            self.filter_clickbait(ctx, check, &mut open).await?;
        }

        // One request per batch of articles with the same open tasks.
        let mut groups: BTreeMap<BTreeSet<Task>, Vec<&ArticleRecord>> = BTreeMap::new();
        for (r, t) in open {
            groups.entry(t).or_default().push(r);
        }
        let batches: Vec<(&BTreeSet<Task>, &[&ArticleRecord])> =
            groups.iter().flat_map(|(t, recs)| recs.chunks(EXTRACT_BATCH).map(move |b| (t, b))).collect();
        self.emit(Progress::Stage { name: "Labels / entities / relevance / headlines", requests: batches.len() });
        let started = std::time::Instant::now();
        let vocabulary: Vec<String> =
            self.db(|s| s.labels()).map(|l| l.into_iter().map(|l| l.name).collect()).unwrap_or_default();
        let reqs: Vec<Request> = batches
            .iter()
            .map(|(tasks, batch)| {
                let per_article_out: u32 = tasks
                    .iter()
                    .map(|t| match t {
                        Task::Labels => 30,
                        Task::Entities => 90,
                        Task::Relevance => 25,
                        Task::Headlines => 30,
                        _ => 0,
                    })
                    .sum();
                let max_out = per_article_out * batch.len() as u32 * 5 + 1_024;
                let mut req = prompts::extraction_request(
                    &config.model,
                    tasks,
                    batch,
                    &vocabulary,
                    config.relevance_criteria.as_deref(),
                    max_out,
                );
                req.reasoning_effort = Some(LIGHT_EFFORT);
                req
            })
            .collect();
        let result = self
            .run_parallel(
                ctx,
                "extraction",
                &reqs,
                |i, v| prompts::validate_extraction(v, batches[i].1, batches[i].0),
                |this, ctx, i, items| {
                    let (tasks, batch) = batches[i];
                    for item in items {
                        if let Some(rec) = batch.iter().find(|r| r.id == item.id) {
                            this.persist_extraction(ctx, rec, &item, tasks);
                        }
                    }
                },
            )
            .await;
        ctx.summary.headlines.extraction_ms = started.elapsed().as_millis() as u64;
        if config.clickbait_check().is_some() {
            tracing::info!(stats = ?ctx.summary.headlines, "headline pipeline");
        }
        result
    }

    // ---- stage 0: clickbait check ------------------------------------------------

    /// Rates the open headlines with the Decisions API and closes the
    /// headline task of those below the threshold. Cached probabilities are
    /// reused, and articles with the same content are rated once.
    async fn filter_clickbait(
        &self,
        ctx: &mut Ctx,
        check: &ClickbaitCheck,
        open: &mut Vec<(&ArticleRecord, BTreeSet<Task>)>,
    ) -> Result<(), Stop> {
        let depth = ctx.config.depth;
        let mut probability: HashMap<&str, f64> = HashMap::new();
        let mut to_rate: Vec<&ArticleRecord> = Vec::new();
        let mut considered = 0;
        for (r, _) in open.iter().filter(|(_, t)| t.contains(&Task::Headlines)) {
            considered += 1;
            let hash = r.content_hash.as_str();
            if probability.contains_key(hash) || to_rate.iter().any(|x| x.content_hash == hash) {
                continue;
            }
            match self.db(|s| s.cached_clickbait(&clickbait_key(hash, check, depth))) {
                Ok(Some(p)) => {
                    probability.insert(hash, p);
                    ctx.summary.headlines.cached += 1;
                }
                _ => to_rate.push(r),
            }
        }
        if considered == 0 {
            return Ok(());
        }
        let started = std::time::Instant::now();
        let rated = self.rate_headlines(ctx, check, &to_rate).await;
        ctx.summary.headlines.check_ms = started.elapsed().as_millis() as u64;
        let by_id: HashMap<i64, &ArticleRecord> = to_rate.iter().map(|r| (r.id, *r)).collect();
        for (id, p) in rated? {
            if let Some(r) = by_id.get(&id) {
                probability.insert(r.content_hash.as_str(), p);
            }
        }

        let stats = &mut ctx.summary.headlines;
        stats.considered += considered;
        for (r, tasks) in open.iter_mut().filter(|(_, t)| t.contains(&Task::Headlines)) {
            match probability.get(r.content_hash.as_str()) {
                Some(p) if !check.is_clickbait(*p) => {
                    tasks.remove(&Task::Headlines);
                    stats.rewrites_avoided += 1;
                }
                Some(_) => stats.clickbait += 1,
                None => stats.fallbacks += 1,
            }
        }
        open.retain(|(_, t)| !t.is_empty());
        let mut note = format!(
            "Clickbait check: {} of {considered} headlines look like clickbait (at least {}%); {} kept without a rewrite.",
            stats.clickbait, check.threshold_percent, stats.rewrites_avoided
        );
        if stats.fallbacks > 0 {
            note.push_str(&format!(" {} could not be rated and go to the rewriting model.", stats.fallbacks));
        }
        self.emit(Progress::Note(note));
        Ok(())
    }

    /// Sends the classification batches, a few at a time, and caches each
    /// probability. Errors end the check without stopping the run; only
    /// pause, cancel and an empty account that also pays for the rewrites
    /// stop it.
    async fn rate_headlines(
        &self,
        ctx: &mut Ctx,
        check: &ClickbaitCheck,
        records: &[&ArticleRecord],
    ) -> Result<HashMap<i64, f64>, Stop> {
        use futures_util::stream::{FuturesUnordered, StreamExt};
        let batches = decisions::batches(records, check.batch_size());
        let bodies: Vec<serde_json::Value> = batches.iter().map(|b| decisions::request(&check.model, b)).collect();
        self.emit(Progress::Stage { name: "Clickbait check", requests: bodies.len() });
        let timeout = std::time::Duration::from_secs(u64::from(check.timeout_secs.max(1)));
        let bodies = &bodies;
        let client = self.check_client.as_ref().unwrap_or(&self.client);
        let send = |i: usize| async move {
            let reply = tokio::select! {
                r = client.decide(&bodies[i], timeout) => Some(r),
                _ = self.control.cancelled() => None,
            };
            (i, reply)
        };
        let mut rated = HashMap::new();
        let mut flights: HashMap<usize, InFlight> = HashMap::new();
        let mut pending = FuturesUnordered::new();
        let mut next = 0;
        let mut done = 0;
        // Set by the first failure: the remaining batches are not sent.
        let mut broken = false;
        let mut stop = None;
        loop {
            while !broken && stop.is_none() && pending.len() < ctx.config.parallel_requests() && next < bodies.len() {
                if let Err(s) = self.check_control() {
                    stop = Some(s);
                    break;
                }
                let input = juttusihti_core::estimate::approx_tokens(&bodies[next].to_string()) * 135 / 100 + 64;
                let output = batches[next].len() as u64 * CHECK_OUTPUT_PER_QUESTION + 256;
                let worst_usage = Usage { input, output, ..Default::default() };
                let worst_cost =
                    ctx.check_price.as_ref().and_then(|p| p.cost(&Usage { output: 0, ..worst_usage }, false));
                match ctx.guard.reserve(worst_usage.total(), worst_cost) {
                    Ok(reservation) => {
                        flights.insert(next, InFlight { reservation, worst_usage, worst_cost });
                        pending.push(send(next));
                        next += 1;
                    }
                    Err(Refusal::Paused) => {
                        stop = Some(Stop::Paused("paused".into()));
                        break;
                    }
                    Err(r) if pending.is_empty() => {
                        self.emit(Progress::Note(format!("Clickbait check skipped: {r}")));
                        broken = true;
                    }
                    // Budget may free up once a request in flight settles.
                    Err(_) => break,
                }
            }
            let Some((i, reply)) = pending.next().await else { break };
            let InFlight { reservation, worst_usage, worst_cost } =
                flights.remove(&i).expect("every request in flight has a reservation");
            ctx.summary.headlines.check_requests += 1;
            let ids: Vec<i64> = batches[i].iter().map(|r| r.id).collect();
            let reply = match reply {
                None => {
                    // Cancelled in flight: the provider may still bill the request.
                    ctx.guard.settle(reservation, None);
                    let op = format!("{CHECK_OPERATION}:cancelled_in_flight");
                    self.ledger_for(ctx, &check.model, &op, &worst_usage, 0, worst_cost);
                    stop.get_or_insert(Stop::Cancelled);
                    continue;
                }
                Some(r) => r.and_then(|v| decisions::parse(&v, &ids)),
            };
            match reply {
                Ok(d) => {
                    let cost = ctx.check_price.as_ref().and_then(|p| p.cost(&Usage { output: 0, ..d.usage }, false));
                    ctx.guard.settle(reservation, Some((d.usage.total(), cost)));
                    self.ledger_for(ctx, &check.model, CHECK_OPERATION, &d.usage, d.reasoning_tokens, cost);
                    if d.refusals > 0 {
                        self.emit(Progress::Note(format!("The clickbait check declined {} headlines.", d.refusals)));
                    }
                    let depth = ctx.config.depth;
                    for r in &batches[i] {
                        let Some(&p) = d.probabilities.get(&r.id) else { continue };
                        rated.insert(r.id, p);
                        ctx.summary.headlines.classified += 1;
                        let key = clickbait_key(&r.content_hash, check, depth);
                        let payload = serde_json::json!({"probability": p, "model": check.model});
                        if let Err(e) = self.db(|s| s.cache_result(&key, r.id, Task::Headlines, &payload)) {
                            tracing::error!(article = r.id, error = %e, "could not cache a clickbait probability");
                        }
                    }
                }
                Err(e) => {
                    if e.possibly_charged() {
                        ctx.guard.settle(reservation, None);
                        let op = format!("{CHECK_OPERATION}:unknown_outcome");
                        self.ledger_for(ctx, &check.model, &op, &worst_usage, 0, worst_cost);
                    } else {
                        ctx.guard.release(reservation);
                    }
                    ctx.summary.headlines.check_errors += 1;
                    tracing::warn!(error = %e, "clickbait check failed");
                    // The rewrite would be refused too: pause, resumable after topping up.
                    if matches!(e, AiError::NoCredit(_)) && !ctx.config.check_on_api_credit() {
                        stop.get_or_insert(no_credit());
                        broken = true;
                        done += 1;
                        continue;
                    }
                    // On a split run the plan still pays for the rewrites.
                    if !broken {
                        self.emit(Progress::Note(if matches!(e, AiError::NoCredit(_)) {
                            "Your OpenAI API account has no credit left, so the clickbait check stopped; headlines \
                                go to the rewriting model without it."
                                .into()
                        } else {
                            format!("Clickbait check failed ({e}); headlines go to the rewriting model without it.")
                        }));
                    }
                    broken = true;
                }
            }
            done += 1;
            self.emit_call(ctx, CHECK_OPERATION, done, bodies.len());
        }
        match stop {
            Some(s) => Err(s),
            None => Ok(rated),
        }
    }

    /// Calls and validates, retrying once when output is unusable.
    async fn call_validated<T>(
        &self,
        ctx: &mut Ctx,
        operation: &'static str,
        req: &Request,
        validate: impl Fn(&serde_json::Value) -> Result<T, String>,
    ) -> Result<Option<T>, Stop> {
        for attempt in 0..=OUTPUT_RETRIES {
            if let Some(v) = self.call(ctx, operation, req).await? {
                match validate(&v) {
                    Ok(t) => return Ok(Some(t)),
                    Err(e) => {
                        tracing::warn!(operation, attempt, error = %e, "model output failed validation");
                        self.emit(Progress::Note(format!("Discarded invalid {operation} output: {e}")));
                    }
                }
            }
        }
        Ok(None)
    }

    /// Sends independent requests, keeping up to the configured number in
    /// flight: a new one starts as soon as any returns. Valid results are
    /// handed to `accept` (and stored) as each returns, so a stop never
    /// loses paid work. Unusable output is retried up to [`OUTPUT_RETRIES`]
    /// times.
    async fn run_parallel<T>(
        &self,
        ctx: &mut Ctx,
        operation: &'static str,
        reqs: &[Request],
        validate: impl Fn(usize, &serde_json::Value) -> Result<T, String>,
        mut accept: impl FnMut(&Self, &mut Ctx, usize, T),
    ) -> Result<(), Stop> {
        use futures_util::stream::{FuturesUnordered, StreamExt};
        let send = |i: usize| async move { (i, self.send(&reqs[i]).await) };
        let mut queue: VecDeque<usize> = (0..reqs.len()).collect();
        let mut attempts = vec![0u32; reqs.len()];
        let mut flights: HashMap<usize, InFlight> = HashMap::new();
        let mut pending = FuturesUnordered::new();
        let mut done = 0;
        let mut stop = None;
        loop {
            while stop.is_none()
                && pending.len() < ctx.config.parallel_requests()
                && let Some(&i) = queue.front()
            {
                match self.begin(ctx, &reqs[i]) {
                    Ok(f) => {
                        queue.pop_front();
                        flights.insert(i, f);
                        pending.push(send(i));
                    }
                    Err(s) if pending.is_empty() => return Err(s),
                    // Budget may free up once a request in flight settles at actual usage.
                    Err(_) => break,
                }
            }
            let Some((i, reply)) = pending.next().await else { break };
            let flight = flights.remove(&i).expect("every request in flight has a reservation");
            match self.finish(ctx, operation, flight, reply) {
                Ok(Some(v)) => match validate(i, &v) {
                    Ok(t) => {
                        accept(self, ctx, i, t);
                        done += 1;
                        self.emit_call(ctx, operation, done, reqs.len());
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(operation, error = %e, "model output failed validation");
                        self.emit(Progress::Note(format!("Discarded invalid {operation} output: {e}")));
                    }
                },
                Ok(None) => {}
                Err(s) => {
                    stop.get_or_insert(s);
                    continue;
                }
            }
            attempts[i] += 1;
            if attempts[i] <= OUTPUT_RETRIES {
                queue.push_back(i);
            } else {
                done += 1;
            }
            self.emit_call(ctx, operation, done, reqs.len());
        }
        match stop {
            Some(s) => Err(s),
            None => Ok(()),
        }
    }

    fn emit_call(&self, ctx: &Ctx, stage: &'static str, done: usize, total: usize) {
        let (tokens, cost) = (ctx.summary.usage.total(), ctx.summary.cost);
        self.emit(Progress::Call { stage, done, total, tokens, cost });
    }

    fn persist_extraction(
        &self,
        ctx: &mut Ctx,
        rec: &ArticleRecord,
        item: &prompts::Extraction,
        tasks: &BTreeSet<Task>,
    ) {
        let config = ctx.config.clone();
        let payload = serde_json::json!({
            "article_id": rec.id,
            "labels": item.labels,
            "entities": item.entities.iter().map(|e| serde_json::json!({
                "name": e.name, "kind": e.kind.as_str(), "role": e.role.as_str(), "evidence": e.evidence,
            })).collect::<Vec<_>>(),
            "relevance": item.relevance.as_ref().map(|(c, r)| serde_json::json!({"class": c.as_str(), "reason": r})),
            "headline": item.headline,
        });
        let suggestion = self.suggestion(ctx, "extraction", payload.clone(), None, None);
        let ctx_session = ctx.session;
        let stored = self.db(|s| {
            let sid = s.record_suggestion(&suggestion)?;
            for name in &item.labels {
                // Prefer the existing vocabulary to avoid label proliferation.
                let label = match s.similar_labels(name)?.into_iter().next() {
                    Some(existing) => existing.id,
                    None => s.create_label(name)?,
                };
                s.tag_article(rec.id, label, Origin::Ai, Some(sid))?;
            }
            for e in &item.entities {
                let entity = s.resolve_entity(e.kind, &e.name)?;
                s.link_entity(rec.id, entity, e.role.as_str(), e.evidence.as_deref(), Origin::Ai, Some(sid))?;
                if e.kind == EntityKind::Place
                    && let Some(role) = e.role.location_role()
                {
                    s.add_location(
                        rec.id,
                        &NewLocation {
                            place_name: e.name.clone(),
                            country: None,
                            // An AI place name is not a verified coordinate.
                            coordinates: None,
                            role,
                            evidence: e.evidence.clone(),
                            confidence: None,
                            coordinate_source: None,
                            ambiguous: false,
                        },
                        Origin::Ai,
                    )?;
                }
            }
            if let Some(h) = &item.headline {
                s.suggest_headline(rec.id, h, Some(ctx_session), Some(sid))?;
            }
            if let Some((class, reason)) = &item.relevance {
                s.record_relevance(rec.id, Some(ctx_session), class.as_str(), Some(reason), Origin::Ai, Some(sid))?;
            }
            for t in tasks {
                let key = article_cache_key(&rec.content_hash, config.provider, &config.model, *t, config.depth);
                s.cache_result(&key, rec.id, *t, &payload)?;
            }
            Ok(())
        });
        match stored {
            Ok(()) => ctx.summary.suggestions += 1,
            Err(e) => tracing::error!(article = rec.id, error = %e, "could not store extraction"),
        }
    }

    // ---- stage 2: related stories ----------------------------------------------------

    fn pair_key(ctx: &Ctx, a: &ArticleRecord, b: &ArticleRecord) -> String {
        sha256_hex(&format!(
            "pair|{}|{}|{:?}|{}|{PROMPT_VERSION}|{:?}",
            a.content_hash, b.content_hash, ctx.config.provider, ctx.config.model, ctx.config.depth
        ))
    }

    async fn related(&self, ctx: &mut Ctx, scope: &Scope) -> Result<(), Stop> {
        let find = |id: i64| scope.records.iter().find(|r| r.id == id);
        let pairs: Vec<(&ArticleRecord, &ArticleRecord)> = scope
            .candidates
            .iter()
            .filter_map(|c| Some((find(c.a)?, find(c.b)?)))
            .filter(|(a, b)| !self.db(|s| s.is_cached(&Self::pair_key(ctx, a, b))).unwrap_or(false))
            .collect();
        let batches: Vec<&[(&ArticleRecord, &ArticleRecord)]> = pairs.chunks(PAIR_BATCH).collect();
        self.emit(Progress::Stage { name: "Related-story validation", requests: batches.len() });
        let reqs: Vec<Request> = batches
            .iter()
            .map(|batch| {
                let mut req = prompts::related_request(&ctx.config.model, batch, (batch.len() as u32) * 250 + 1_024);
                req.reasoning_effort = Some(LIGHT_EFFORT);
                req
            })
            .collect();
        self.run_parallel(
            ctx,
            "related",
            &reqs,
            |i, v| prompts::validate_related(v, batches[i]),
            |this, ctx, _, verdicts| {
                for v in verdicts {
                    let (Some(a), Some(b)) = (find(v.a), find(v.b)) else { continue };
                    this.persist_relationship(ctx, a, b, &v);
                }
            },
        )
        .await
    }

    fn persist_relationship(&self, ctx: &mut Ctx, a: &ArticleRecord, b: &ArticleRecord, v: &prompts::PairVerdict) {
        let suggestion = self.suggestion(
            ctx,
            "relationship",
            serde_json::json!({"a": v.a, "b": v.b, "relation": v.kind.map(|k| k.as_str()).unwrap_or("unrelated")}),
            Some(v.confidence),
            v.evidence.clone(),
        );
        let key = Self::pair_key(ctx, a, b);
        let stored = self.db(|s| {
            let sid = s.record_suggestion(&suggestion)?;
            if let Some(kind) = v.kind {
                s.relate(v.a, v.b, kind, Origin::Ai, v.evidence.as_deref(), Some(v.confidence), Some(sid))?;
            }
            s.cache_result(&key, v.a, Task::Related, &serde_json::json!({"suggestion": sid}))
        });
        match stored {
            Ok(()) => ctx.summary.suggestions += 1,
            Err(e) => tracing::error!(error = %e, "could not store relationship"),
        }
    }

    // ---- stage 3: summary -------------------------------------------------------------

    async fn summarize(&self, ctx: &mut Ctx, scope: &Scope) -> Result<(), Stop> {
        let key = sha256_hex(&format!("summary|session|{}|{PROMPT_VERSION}", ctx.session));
        if self.db(|s| s.is_cached(&key)).unwrap_or(false) || scope.records.is_empty() {
            return Ok(());
        }
        let records: Vec<&ArticleRecord> = scope.records.iter().collect();
        let allowed: HashSet<i64> = records.iter().map(|r| r.id).collect();
        let chunks: Vec<&[&ArticleRecord]> = records.chunks(SUMMARY_CHUNK).collect();
        self.emit(Progress::Stage { name: "Research summary", requests: chunks.len() + usize::from(chunks.len() > 1) });
        let summary = if chunks.len() == 1 {
            let req = prompts::summary_request(&ctx.config.model, chunks[0], 10_024);
            self.call_validated(ctx, "summary", &req, |v| prompts::validate_summary(v, &allowed)).await?
        } else {
            let reqs: Vec<Request> =
                chunks.iter().map(|chunk| prompts::summary_request(&ctx.config.model, chunk, 3_524)).collect();
            let mut parts: Vec<Option<Summary>> = vec![None; reqs.len()];
            self.run_parallel(
                ctx,
                "summary_chunk",
                &reqs,
                |_, v| prompts::validate_summary(v, &allowed),
                |_, _, i, p| parts[i] = Some(p),
            )
            .await?;
            // Partials stay in chronological chunk order.
            let partials: Vec<Summary> = parts.into_iter().flatten().collect();
            if partials.is_empty() {
                None
            } else {
                let req = prompts::final_summary_request(&ctx.config.model, &partials, 10_024);
                self.call_validated(ctx, "summary", &req, |v| prompts::validate_summary(v, &allowed)).await?
            }
        };
        let Some(summary) = summary else {
            return Err(Stop::Failed("the summary could not be generated".into()));
        };
        let payload = serde_json::to_value(&summary).unwrap_or_default();
        let suggestion = self.suggestion(ctx, "summary", payload.clone(), None, None);
        let first = records[0].id;
        let stored = self.db(|s| {
            s.record_suggestion(&suggestion)?;
            s.cache_result(&key, first, Task::Summary, &payload)
        });
        match stored {
            Ok(()) => ctx.summary.suggestions += 1,
            Err(e) => return Err(Stop::Failed(format!("could not store summary: {e}"))),
        }
        Ok(())
    }
}

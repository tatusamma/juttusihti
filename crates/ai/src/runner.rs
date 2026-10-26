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
//! Source articles are never modified.

use std::collections::{BTreeSet, HashSet};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use juttusihti_core::analysis::{
    AnalysisConfig, ArticleRecord, PROMPT_VERSION, Scope, Task, cache_key as article_cache_key,
};
use juttusihti_core::budget::{Guard, Refusal};
use juttusihti_core::normalize::sha256_hex;
use juttusihti_core::pricing::{Billing, Catalog, Micros, Price, Usage};
use juttusihti_core::research::{NewLocation, NewSuggestion};
use juttusihti_core::store::Store;
use juttusihti_core::{EntityKind, Origin, SessionStatus};
use tokio::sync::{Notify, mpsc};

use crate::prompts::{self, Summary};
use crate::{AiError, Client, Outcome, Request};

const EXTRACT_BATCH: usize = 20;
const PAIR_BATCH: usize = 10;
const SUMMARY_CHUNK: usize = 40;
/// Retries of a batch whose output failed validation or was truncated.
const OUTPUT_RETRIES: u32 = 1;

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
    fn state(&self) -> u8 {
        self.0.0.load(Ordering::SeqCst)
    }
    async fn cancelled(&self) {
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
}

enum Stop {
    Paused(String),
    Cancelled,
    Failed(String),
}

pub struct Runner {
    db: Arc<Mutex<Store>>,
    client: Client,
    catalog: Catalog,
    control: Control,
    progress: Option<mpsc::UnboundedSender<Progress>>,
}

struct Ctx {
    session: i64,
    config: AnalysisConfig,
    price: Option<Price>,
    guard: Guard,
    summary: RunSummary,
}

impl Runner {
    pub fn new(db: Arc<Mutex<Store>>, client: Client, catalog: Catalog, control: Control) -> Self {
        Self { db, client, catalog, control, progress: None }
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
        let price = self.catalog.price_for(config.provider, &config.model, config.billing, Utc::now().date_naive());
        let spent = self.db(|s| s.session_usage(session))?;
        let guard = Guard::new(config.limits, spent);
        self.control.reset();
        self.db(|s| s.set_session_status(session, SessionStatus::Running, None))?;

        let mut ctx =
            Ctx { session, config, price, guard, summary: RunSummary { cost: Some(0), ..Default::default() } };
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

        let result = tokio::select! {
            r = self.client.complete(req) => Some(r),
            _ = self.control.cancelled() => None,
        };
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
                    Outcome::NotJson(_) => {
                        self.emit(Progress::Note(format!("A {operation} response was not valid JSON.")));
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
                    AiError::PlanLimit(m) => Err(Stop::Paused(format!(
                        "ChatGPT plan usage limit reached ({m}). Check ChatGPT settings → Usage, then resume."
                    ))),
                    AiError::Status { status: 401 | 403, .. } if ctx.config.billing == Billing::ChatGptPlan => Err(
                        Stop::Failed("OpenAI rejected the ChatGPT sign-in; sign in again under AI Providers.".into()),
                    ),
                    AiError::Status { status: 401 | 403, .. } => {
                        Err(Stop::Failed("The provider rejected the API key.".into()))
                    }
                    other => Err(Stop::Failed(other.to_string())),
                }
            }
        }
    }

    fn ledger(&self, ctx: &mut Ctx, operation: &str, usage: &Usage, reasoning: u64, cost: Option<Micros>) {
        ctx.summary.calls += 1;
        ctx.summary.usage += *usage;
        ctx.summary.cost = match (ctx.summary.cost, cost) {
            (Some(a), Some(b)) => Some(a + b),
            _ => None,
        };
        let (session, provider, model) = (ctx.session, ctx.config.provider, ctx.config.model.clone());
        if let Err(e) = self.db(|s| s.record_usage(Some(session), provider, &model, operation, usage, reasoning, cost))
        {
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
        // Re-check the cache: a resumed run skips articles finished earlier.
        let config = ctx.config.clone();
        let pending: Vec<&ArticleRecord> = scope
            .records
            .iter()
            .filter(|r| scope.needs_processing.contains(&r.id))
            .filter(|r| {
                !tasks.iter().all(|t| {
                    let key = article_cache_key(&r.content_hash, config.provider, &config.model, *t, config.depth);
                    self.db(|s| s.is_cached(&key)).unwrap_or(false)
                })
            })
            .collect();
        let batches: Vec<&[&ArticleRecord]> = pending.chunks(EXTRACT_BATCH).collect();
        self.emit(Progress::Stage { name: "Labels / entities / relevance", requests: batches.len() });
        for (i, batch) in batches.iter().enumerate() {
            let vocabulary: Vec<String> =
                self.db(|s| s.labels()).map(|l| l.into_iter().map(|l| l.name).collect()).unwrap_or_default();
            let per_article_out: u32 = tasks
                .iter()
                .map(|t| match t {
                    Task::Labels => 30,
                    Task::Entities => 90,
                    Task::Relevance => 25,
                    _ => 0,
                })
                .sum();
            let max_out = per_article_out * batch.len() as u32 * 5 + 1_024;
            let req = prompts::extraction_request(
                &config.model,
                tasks,
                batch,
                &vocabulary,
                config.relevance_criteria.as_deref(),
                max_out,
            );
            let items =
                self.call_validated(ctx, "extraction", &req, |v| prompts::validate_extraction(v, batch, tasks)).await?;
            if let Some(items) = items {
                for item in items {
                    let Some(rec) = batch.iter().find(|r| r.id == item.id) else { continue };
                    self.persist_extraction(ctx, rec, &item, tasks);
                }
            }
            let (tokens, cost) = (ctx.summary.usage.total(), ctx.summary.cost);
            self.emit(Progress::Call { stage: "extraction", done: i + 1, total: batches.len(), tokens, cost });
        }
        Ok(())
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
        for (i, batch) in batches.iter().enumerate() {
            let req = prompts::related_request(&ctx.config.model, batch, (batch.len() as u32) * 250 + 1_024);
            let verdicts = self.call_validated(ctx, "related", &req, |v| prompts::validate_related(v, batch)).await?;
            for v in verdicts.unwrap_or_default() {
                let (Some(a), Some(b)) = (find(v.a), find(v.b)) else { continue };
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
            let (tokens, cost) = (ctx.summary.usage.total(), ctx.summary.cost);
            self.emit(Progress::Call { stage: "related", done: i + 1, total: batches.len(), tokens, cost });
        }
        Ok(())
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
            let mut partials: Vec<Summary> = Vec::new();
            for chunk in &chunks {
                let req = prompts::summary_request(&ctx.config.model, chunk, 3_524);
                if let Some(p) =
                    self.call_validated(ctx, "summary_chunk", &req, |v| prompts::validate_summary(v, &allowed)).await?
                {
                    partials.push(p);
                }
            }
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

//! Runs a fact check: extraction, then each claim against evidence, every
//! call reserved against the check's own spending limit.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use chrono::Utc;
use futures_util::StreamExt;
use juttusihti_core::budget::{Guard, Limits, Refusal, Reservation, Totals};
use juttusihti_core::estimate::approx_tokens;
use juttusihti_core::factcheck::{
    ClaimResult, FACT_CHECK_PROMPT_VERSION, FactCheck, Gathered, SEARCH_TOKENS_CONSERVATIVE, SEARCHES_PER_CLAIM,
    Source, SourceKind, Verdict, bound_text, judge_sources,
};
use juttusihti_core::pricing::{Billing, Catalog, Micros, Price, Usage};
use juttusihti_core::store::Store;
use tokio::sync::mpsc;

use super::{
    ArticleForCheck, ExtractedClaim, Extraction, claim_request, extract_request, parse_extraction, parse_judgement,
};
use crate::sources::{PublishedCheck, Sources, WikiPage};
use crate::{AiError, Client, Control, Outcome, Reply, Request, WebSearch, WebSource, is_tool_unsupported};

/// Claims checked at the same time.
const CLAIMS_IN_FLIGHT: usize = 3;
/// Published fact checks passed to the model per claim.
const MAX_PUBLISHED: usize = 5;
/// Room in a claim's reservation for the published fact checks and
/// Wikipedia extracts, which arrive after the reservation is made.
const LOOKUP_TOKENS: u64 = 2_000;
const TOO_LOW: &str = "The fact check limit is too low for this article.";
const UNREADABLE: &str = "Could not read the article's claims.";
const CLAIM_FAILED: &str = "The check could not be completed for this claim.";
const UNCONFIRMED: &str = "No independent source could be confirmed for this verdict.";
const UNKNOWN_PRICE: &str = "The price of this model is unknown, so the fact check cannot keep to its limit.";
/// Bound for each published fact check field passed to the model, so the
/// lookup room in a claim's reservation holds.
const MAX_PUBLISHED_FIELD: usize = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactCheckProgress {
    Extracting,
    Checking { done: usize, total: usize },
}

#[derive(Debug, Clone)]
pub struct FactCheckJob {
    pub article: ArticleForCheck,
    pub model: String,
    /// This check's own spending limit.
    pub limit: Micros,
}

#[derive(Debug, thiserror::Error)]
pub enum FactCheckError {
    #[error("The fact check was cancelled.")]
    Cancelled,
    #[error("{0}")]
    Ai(String),
    #[error("Could not save the fact check: {0}")]
    Store(String),
}

/// Checks one article: extracts its claims, checks each against evidence
/// (at most three at a time, most important first) and saves the result as
/// the article's only check. Every call is reserved against `job.limit`
/// first; claims that no longer fit are marked not checked. A cancelled run
/// saves nothing, but its usage stays in the ledger.
pub async fn run_fact_check(
    job: FactCheckJob,
    client: &Client,
    sources: &Sources,
    catalog: &Catalog,
    db: Arc<Mutex<Store>>,
    control: &Control,
    progress: mpsc::UnboundedSender<FactCheckProgress>,
) -> Result<FactCheck, FactCheckError> {
    let Some(price) = catalog.price_for(client.provider(), &job.model, client.billing(), Utc::now().date_naive())
    else {
        return Err(FactCheckError::Ai(UNKNOWN_PRICE.into()));
    };
    // Without a search price the limit cannot cover searches: check without them.
    let search_priced = price.search_cost(SEARCHES_PER_CLAIM).is_some();
    if !search_priced {
        tracing::info!(model = %job.model, "web search price unknown; checking without search");
    }
    let limits = Limits { max_cost: Some(job.limit), max_tokens: None, max_calls: None, allow_unknown_cost: false };
    let run = Run {
        job: &job,
        client,
        sources,
        db: &db,
        control,
        price,
        guard: Mutex::new(Guard::new(limits, Totals::default())),
        cost: Mutex::new(Some(0)),
        web_search: AtomicBool::new(search_priced),
        limit_reached: AtomicBool::new(false),
        stopped: Mutex::new(None),
    };
    let _ = progress.send(FactCheckProgress::Extracting);
    let extraction = run.extract().await?;
    let claims = run.check_claims(&extraction, &progress).await?;
    let fc = FactCheck {
        article_id: job.article.article_id,
        checked_at: juttusihti_core::now(),
        provider: client.provider(),
        model: job.model.clone(),
        billing: client.billing(),
        prompt_version: FACT_CHECK_PROMPT_VERSION,
        language: extraction.language,
        web_search: run.web_search.load(Ordering::SeqCst),
        claims,
        guidelines: extraction.guidelines,
        cost: *lock(&run.cost),
    };
    lock(&db).save_fact_check(&fc).map_err(|e| FactCheckError::Store(e.to_string()))?;
    Ok(fc)
}

/// The budget, ledger and switches shared by one run's calls.
struct Run<'a> {
    job: &'a FactCheckJob,
    client: &'a Client,
    sources: &'a Sources,
    db: &'a Mutex<Store>,
    control: &'a Control,
    price: Price,
    guard: Mutex<Guard>,
    /// Sum of settled costs; `None` once any cost is unknown.
    cost: Mutex<Option<Micros>>,
    /// Cleared for this and all later claims once the model rejects the search tool.
    web_search: AtomicBool,
    /// Set when a claim no longer fit the limit; later claims are not started.
    limit_reached: AtomicBool,
    /// Why the run stops: an account-level provider error on a claim. Later
    /// claims are not started and the run fails without saving.
    stopped: Mutex<Option<String>>,
}

/// A reserved call's worst case.
struct Held {
    reservation: Reservation,
    worst: Usage,
    worst_cost: Option<Micros>,
}

/// How a claim's first call was reserved, decided in importance order.
enum Start {
    Go(Held),
    LimitReached,
    Stop(FactCheckError),
}

/// Locks without holding the lock across an await; a poisoned lock still
/// holds consistent data here (every critical section is one call).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Extra input tokens and searches a claim call may use beyond its prompt.
fn search_allowance(searching: bool) -> (u64, u32) {
    if searching { (u64::from(SEARCHES_PER_CLAIM) * SEARCH_TOKENS_CONSERVATIVE, SEARCHES_PER_CLAIM) } else { (0, 0) }
}

/// Whether a refusal means the limit is used up (rather than a price problem).
fn is_limit(r: &Refusal) -> bool {
    matches!(r, Refusal::Cost { .. } | Refusal::Tokens { .. } | Refusal::Calls)
}

fn refusal_error(r: &Refusal) -> FactCheckError {
    FactCheckError::Ai(match r {
        Refusal::UnknownCost => UNKNOWN_PRICE.into(),
        other => format!("The fact check stopped: {other}"),
    })
}

/// A provider error that fails every claim alike (credit, plan, sign-in, key).
fn is_account_error(e: &AiError) -> bool {
    matches!(
        e,
        AiError::NoCredit(_) | AiError::PlanLimit(_) | AiError::Auth(_) | AiError::Status { status: 401 | 403, .. }
    )
}

impl Run<'_> {
    fn cancelled(&self) -> Result<(), FactCheckError> {
        if self.control.is_cancelled() { Err(FactCheckError::Cancelled) } else { Ok(()) }
    }

    fn call_cost(&self, usage: &Usage, searches: u32) -> Option<Micros> {
        Some(self.price.cost(usage, false)? + self.price.search_cost(searches)?)
    }

    /// Reserves a request's worst case: its prompt with a margin, the
    /// output limit, plus `extra_input` tokens and `searches` searches.
    fn reserve(&self, req: &Request, extra_input: u64, searches: u32) -> Result<Held, Refusal> {
        let prompt = approx_tokens(&req.system) + approx_tokens(&req.user) + approx_tokens(&req.schema.to_string());
        let worst = Usage {
            input: prompt * 135 / 100 + 64 + extra_input,
            output: u64::from(req.max_output_tokens),
            ..Default::default()
        };
        let worst_cost = self.call_cost(&worst, searches);
        let reservation = lock(&self.guard).reserve(worst.total(), worst_cost)?;
        Ok(Held { reservation, worst, worst_cost })
    }

    /// Sends a request; `None` if the run was cancelled while waiting.
    async fn send(&self, req: &Request) -> Option<Result<Reply, AiError>> {
        tokio::select! {
            r = self.client.complete(req) => Some(r),
            _ = self.control.cancelled() => None,
        }
    }

    /// Settles a returned (or abandoned) call in the budget and the ledger.
    /// A call cancelled in flight, or one whose outcome is unknown, may
    /// still be billed, so it is charged at its worst case.
    fn finish(
        &self,
        held: Held,
        operation: &str,
        result: Option<Result<Reply, AiError>>,
    ) -> Result<Result<Reply, AiError>, FactCheckError> {
        let Held { reservation, worst, worst_cost } = held;
        let Some(result) = result else {
            lock(&self.guard).settle(reservation, None);
            self.ledger(&format!("{operation}:cancelled_in_flight"), &worst, 0, worst_cost);
            return Err(FactCheckError::Cancelled);
        };
        match &result {
            Ok(reply) => {
                // Gemini cannot cap its searches, so this may exceed the reservation.
                let cost = self.call_cost(&reply.usage, reply.web.searches);
                lock(&self.guard).settle(reservation, Some((reply.usage.total(), cost)));
                self.ledger(operation, &reply.usage, reply.reasoning_tokens, cost);
            }
            Err(e) if e.possibly_charged() => {
                lock(&self.guard).settle(reservation, None);
                self.ledger(&format!("{operation}:unknown_outcome"), &worst, 0, worst_cost);
            }
            Err(_) => lock(&self.guard).release(reservation),
        }
        Ok(result)
    }

    fn ledger(&self, operation: &str, usage: &Usage, reasoning: u64, cost: Option<Micros>) {
        {
            let mut total = lock(&self.cost);
            *total = total.zip(cost).map(|(a, b)| a + b);
        }
        let (provider, model) = (self.client.provider(), &self.job.model);
        if let Err(e) = lock(self.db).record_usage(None, provider, model, operation, usage, reasoning, cost) {
            tracing::error!(error = %e, "could not write usage ledger");
        }
    }

    /// Reads the article's claims and guideline checks, retrying once when
    /// the output is unusable.
    async fn extract(&self) -> Result<Extraction, FactCheckError> {
        let article = &self.job.article;
        let req = extract_request(&self.job.model, article);
        for _ in 0..2 {
            self.cancelled()?;
            let held = self.reserve(&req, 0, 0).map_err(|r| {
                tracing::info!(refusal = %r, "fact check extraction refused by its limit");
                if is_limit(&r) { FactCheckError::Ai(TOO_LOW.into()) } else { refusal_error(&r) }
            })?;
            let reply = self
                .finish(held, "fact_check_extract", self.send(&req).await)?
                .map_err(|e| FactCheckError::Ai(provider_message(&e, self.client.billing())))?;
            match reply.outcome {
                Outcome::Json(v) => match parse_extraction(&v, article) {
                    Ok(extraction) => return Ok(extraction),
                    Err(why) => tracing::warn!(%why, "fact check extraction did not validate"),
                },
                Outcome::Refused(why) => {
                    return Err(FactCheckError::Ai(format!("The provider declined to check this article: {why}")));
                }
                Outcome::Truncated => tracing::warn!("fact check extraction hit its output limit"),
                Outcome::NotJson(text) => tracing::warn!(%text, "fact check extraction was not JSON"),
            }
        }
        Err(FactCheckError::Ai(UNREADABLE.into()))
    }

    /// Checks every claim and returns the results in importance order.
    async fn check_claims(
        &self,
        extraction: &Extraction,
        progress: &mpsc::UnboundedSender<FactCheckProgress>,
    ) -> Result<Vec<ClaimResult>, FactCheckError> {
        let total = extraction.claims.len();
        if total == 0 {
            return Ok(Vec::new());
        }
        let _ = progress.send(FactCheckProgress::Checking { done: 0, total });
        let lang = extraction.language.as_str();
        // The stream starts claims in order, so reservations are made most important first.
        // It yields indices, not references, so the run's future stays `Send`.
        let mut checks = futures_util::stream::iter(0..total)
            .map(|i| {
                let claim = &extraction.claims[i];
                let start = self.start_claim(lang, claim);
                async move { (i, self.check_claim(lang, claim, start).await) }
            })
            .buffer_unordered(CLAIMS_IN_FLIGHT);
        let mut done = Vec::with_capacity(total);
        while let Some(result) = checks.next().await {
            done.push(result);
            let _ = progress.send(FactCheckProgress::Checking { done: done.len(), total });
        }
        done.sort_by_key(|(i, _)| *i);
        done.into_iter().map(|(_, r)| r).collect()
    }

    /// Reserves a claim's first call before it starts.
    fn start_claim(&self, lang: &str, claim: &ExtractedClaim) -> Start {
        if self.control.is_cancelled() {
            return Start::Stop(FactCheckError::Cancelled);
        }
        if let Some(why) = lock(&self.stopped).clone() {
            return Start::Stop(FactCheckError::Ai(why));
        }
        if self.limit_reached.load(Ordering::SeqCst) {
            return Start::LimitReached;
        }
        let searching = self.web_search.load(Ordering::SeqCst);
        let (search_tokens, searches) = search_allowance(searching);
        let req = self.claim_req(lang, claim, &[], &[], searching);
        match self.reserve(&req, LOOKUP_TOKENS + search_tokens, searches) {
            Ok(held) => Start::Go(held),
            Err(r) if !is_limit(&r) => Start::Stop(refusal_error(&r)),
            Err(r) => {
                tracing::info!(refusal = %r, "fact check limit reached; remaining claims are not checked");
                self.limit_reached.store(true, Ordering::SeqCst);
                Start::LimitReached
            }
        }
    }

    fn claim_req(
        &self,
        lang: &str,
        claim: &ExtractedClaim,
        published: &[PublishedCheck],
        wiki: &[WikiPage],
        searching: bool,
    ) -> Request {
        let search = searching.then_some(WebSearch { max_uses: SEARCHES_PER_CLAIM });
        claim_request(&self.job.model, &self.job.article, lang, claim, published, wiki, search)
    }

    async fn check_claim(
        &self,
        lang: &str,
        claim: &ExtractedClaim,
        start: Start,
    ) -> Result<ClaimResult, FactCheckError> {
        let mut held = match start {
            Start::Go(held) => Some(held),
            Start::Stop(e) => return Err(e),
            Start::LimitReached => return Ok(not_checked(claim)),
        };
        let (published, wiki) = tokio::select! {
            found = self.lookups(lang, claim) => found,
            _ = self.control.cancelled() => {
                if let Some(h) = held.take() {
                    lock(&self.guard).release(h.reservation);
                }
                return Err(FactCheckError::Cancelled);
            }
        };
        let mut failures = 0;
        loop {
            let searching = self.web_search.load(Ordering::SeqCst);
            let req = self.claim_req(lang, claim, &published, &wiki, searching);
            let h = match held.take() {
                Some(h) => h,
                None => {
                    self.cancelled()?;
                    let (search_tokens, searches) = search_allowance(searching);
                    match self.reserve(&req, search_tokens, searches) {
                        Ok(h) => h,
                        Err(r) if !is_limit(&r) => return Err(refusal_error(&r)),
                        // A paid attempt already failed: the check could not be completed.
                        Err(_) if failures > 0 => return Ok(failed(claim)),
                        Err(_) => return Ok(not_checked(claim)),
                    }
                }
            };
            match self.finish(h, "fact_check_claim", self.send(&req).await)? {
                Err(e) if searching && is_tool_unsupported(&e) => {
                    tracing::info!(error = %e, "the model cannot search the web; checking without search");
                    self.web_search.store(false, Ordering::SeqCst);
                }
                Err(e) if is_account_error(&e) => {
                    let why = provider_message(&e, self.client.billing());
                    tracing::warn!(error = %e, "fact check stopped by an account error");
                    lock(&self.stopped).get_or_insert_with(|| why.clone());
                    return Err(FactCheckError::Ai(why));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "fact check claim request failed");
                    return Ok(failed(claim));
                }
                Ok(reply) => {
                    if let Some(result) = self.judged(claim, &reply, &published, &wiki) {
                        return Ok(result);
                    }
                    failures += 1;
                    if failures >= 2 {
                        return Ok(failed(claim));
                    }
                }
            }
        }
    }

    /// Published fact checks and Wikipedia pages for a claim. A failed
    /// lookup only means less evidence.
    async fn lookups(&self, lang: &str, claim: &ExtractedClaim) -> (Vec<PublishedCheck>, Vec<WikiPage>) {
        let (published, wiki) = futures_util::future::join(
            self.sources.fact_checks(&claim.query, lang),
            self.sources.wikipedia(&claim.query, lang),
        )
        .await;
        let mut published = published.unwrap_or_else(|e| {
            tracing::debug!(error = %e, "published fact check lookup failed");
            Vec::new()
        });
        published.truncate(MAX_PUBLISHED);
        for p in &mut published {
            for field in [&mut p.claim_text, &mut p.title, &mut p.publisher, &mut p.rating] {
                *field = bound_text(field, MAX_PUBLISHED_FIELD);
            }
            if let Some(c) = &mut p.claimant {
                *c = bound_text(c, MAX_PUBLISHED_FIELD);
            }
        }
        let wiki = wiki.unwrap_or_else(|e| {
            tracing::debug!(error = %e, "Wikipedia lookup failed");
            Vec::new()
        });
        (published, wiki)
    }

    /// The claim's result from a reply, or `None` when the output is unusable.
    fn judged(
        &self,
        claim: &ExtractedClaim,
        reply: &Reply,
        published: &[PublishedCheck],
        wiki: &[WikiPage],
    ) -> Option<ClaimResult> {
        let judgement = match &reply.outcome {
            Outcome::Json(v) => {
                parse_judgement(v).map_err(|why| tracing::warn!(%why, "claim verdict did not validate"))
            }
            other => {
                tracing::warn!(outcome = ?other, "claim check output was unusable");
                Err(())
            }
        }
        .ok()?;
        let gathered = gathered(published, wiki, &reply.web.sources);
        let (verdict, sources) =
            judge_sources(judgement.verdict, &judgement.cited, &gathered, self.job.article.url.as_deref());
        // A dropped verdict's reason argued for it; it no longer applies.
        let reason = if verdict == judgement.verdict { judgement.reason } else { UNCONFIRMED.into() };
        Some(ClaimResult { quote: claim.quote.clone(), verdict, reason, sources })
    }
}

fn not_checked(claim: &ExtractedClaim) -> ClaimResult {
    ClaimResult {
        quote: claim.quote.clone(),
        verdict: Verdict::NotChecked,
        reason: Verdict::NotChecked.label().into(),
        sources: Vec::new(),
    }
}

fn failed(claim: &ExtractedClaim) -> ClaimResult {
    ClaimResult {
        quote: claim.quote.clone(),
        verdict: Verdict::Unverified,
        reason: CLAIM_FAILED.into(),
        sources: Vec::new(),
    }
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// Everything a claim's verdict may cite. Published checks come first so
/// their publisher and rating win when a search finds the same page.
fn gathered(published: &[PublishedCheck], wiki: &[WikiPage], web: &[WebSource]) -> Gathered {
    let mut g = Gathered::new();
    for p in published {
        g.add(Source {
            url: p.url.clone(),
            title: non_empty(&p.title).unwrap_or_else(|| p.publisher.clone()),
            kind: SourceKind::FactCheck,
            publisher: non_empty(&p.publisher),
            rating: non_empty(&p.rating),
        });
    }
    for w in wiki {
        g.add(Source {
            url: w.url.clone(),
            title: w.title.clone(),
            kind: SourceKind::Wikipedia,
            publisher: None,
            rating: None,
        });
    }
    for s in web {
        let kind = if is_wikipedia(&s.url) { SourceKind::Wikipedia } else { SourceKind::Web };
        g.add(Source { url: s.url.clone(), title: s.title.clone(), kind, publisher: None, rating: None });
    }
    g
}

fn is_wikipedia(raw: &str) -> bool {
    url::Url::parse(raw.trim())
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .is_some_and(|h| h == "wikipedia.org" || h.ends_with(".wikipedia.org"))
}

/// A provider failure in words the user can act on.
fn provider_message(e: &AiError, billing: Billing) -> String {
    match e {
        AiError::NoCredit(_) => format!("{}, then check again.", crate::NO_CREDIT),
        AiError::PlanLimit(m) => format!("ChatGPT plan usage limit reached ({m})."),
        AiError::Status { status: 401 | 403, .. } if billing == Billing::ChatGptPlan => {
            "OpenAI rejected the ChatGPT sign-in; sign in again under AI setup.".into()
        }
        AiError::Status { status: 401 | 403, .. } => "The provider rejected the API key.".into(),
        other => format!("The provider request failed: {other}"),
    }
}

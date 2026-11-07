//! Fact check of the article being read: the action button, the consent
//! panel with its estimate, the progress of a run and the result card.
//! Nothing is sent before the panel's "Check with …" button is pressed.

use std::sync::{Arc, Mutex};

use chrono::Local;
use iced::widget::{button, column, container, row, text, tooltip};
use iced::{Center, Element, Fill, Task as Cmd};
use juttusihti_ai::factcheck::{ArticleForCheck, FactCheckJob, FactCheckProgress, run_fact_check};
use juttusihti_ai::sources::Sources;
use juttusihti_ai::{Client, Control, RetryPolicy};
use juttusihti_core::ArticleId;
use juttusihti_core::ai_defaults::{AiDefaults, Tier, recommended};
use juttusihti_core::factcheck::{
    FactCheck, FactCheckEstimate, GuidelineCheck, GuidelineStatus, Source, SourceKind, Verdict, estimate,
};
use juttusihti_core::pricing::{Billing, Catalog, Micros, Price, Provider, format_usd};
use juttusihti_core::store::Store;

use crate::db::Db;
use crate::icons::{Icon, icon};
use crate::research::cost_text;
use crate::style::{self, SEMIBOLD, SIZE_BODY, SIZE_SMALL, SPACE};
use crate::{ai_setup, format, research, secrets};

/// Shortest article text worth checking, in characters.
pub const MIN_CHECK_CHARS: usize = 300;

/// The card's fixed note.
const NOTE: &str = "AI-assisted check. Verify the sources before relying on it.";

/// Whether the article has enough real text to check: fetched full text or
/// feed content (not just a teaser) of at least [`MIN_CHECK_CHARS`].
pub fn can_check(has_fulltext: bool, feed_content: Option<&str>, body: &str) -> bool {
    (has_fulltext || feed_content.is_some_and(|c| !c.is_empty())) && body.chars().count() >= MIN_CHECK_CHARS
}

/// The defaults' provider with its Quality model; an explicit model only
/// when the defaults are on the Quality tier.
fn fact_check_model(d: &AiDefaults) -> String {
    match (&d.model, d.tier) {
        (Some(m), Tier::Quality) => m.clone(),
        _ => recommended(d.provider, Tier::Quality).model.to_string(),
    }
}

/// What a check would send and cost, worked out when the panel opens.
#[derive(Debug, Clone)]
pub struct Plan {
    pub article: ArticleForCheck,
    pub provider: Provider,
    pub billing: Billing,
    pub model: String,
    pub estimate: FactCheckEstimate,
    pub limit: Micros,
    pub has_google: bool,
    /// False when the model's token price is known but its web search price
    /// is not: the check then runs without web search.
    pub web_search: bool,
    pub ready: Result<(), String>,
}

impl Plan {
    /// Credentials are in place and, on an API key, the cost is known.
    fn can_send(&self) -> bool {
        self.ready.is_ok() && (self.billing == Billing::ChatGptPlan || self.estimate.expected.is_some())
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Open(ArticleId),
    Planned(u64, Result<Box<Plan>, String>),
    /// The consent: starts the check.
    Send,
    /// Stops a running check, or closes the panel before sending.
    Cancel,
    Close,
    Progress(FactCheckProgress),
    /// The app reloads the preview on success.
    Finished(ArticleId, Result<Box<FactCheck>, String>),
    OpenLink(String),
    /// Handled by the app, which knows the article shown.
    ToggleCard,
}

enum Panel {
    Planning,
    Ready(Box<Plan>),
    Failed(String),
}

struct Running {
    article: ArticleId,
    control: Control,
    progress: Option<FactCheckProgress>,
}

pub struct State {
    db: Db,
    catalog: Catalog,
    generation: u64,
    panel: Option<(ArticleId, Panel)>,
    run: Option<Running>,
    /// The article whose card is expanded.
    expanded: Option<ArticleId>,
}

impl State {
    pub fn new(db: Db) -> Self {
        Self { db, catalog: Catalog::builtin(), generation: 0, panel: None, run: None, expanded: None }
    }

    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    pub fn card_collapsed(&self, article: ArticleId) -> bool {
        self.expanded != Some(article)
    }

    pub fn toggle_card(&mut self, article: ArticleId) {
        self.expanded = if self.card_collapsed(article) { Some(article) } else { None };
    }

    pub fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Open(id) => {
                if self.run.is_some() {
                    return Cmd::none();
                }
                self.generation += 1;
                let generation = self.generation;
                self.panel = Some((id, Panel::Planning));
                let (db, catalog) = (self.db.clone(), self.catalog.clone());
                return Cmd::perform(plan(db, catalog, id), move |r| Msg::Planned(generation, r));
            }
            Msg::Planned(g, r) if g == self.generation => {
                if let Some((_, panel)) = self.panel.as_mut() {
                    *panel = match r {
                        Ok(p) => Panel::Ready(p),
                        Err(e) => Panel::Failed(e),
                    };
                }
            }
            Msg::Planned(..) => {}
            Msg::Send => {
                let Some((id, Panel::Ready(plan))) = &self.panel else { return Cmd::none() };
                if self.run.is_some() || !plan.can_send() {
                    return Cmd::none();
                }
                let control = Control::default();
                self.run = Some(Running { article: *id, control: control.clone(), progress: None });
                return Cmd::stream(run(plan, self.db.shared(), self.catalog.clone(), control));
            }
            Msg::Cancel => match &self.run {
                Some(r) => r.control.cancel(),
                None => self.panel = None,
            },
            Msg::Close => self.panel = None,
            Msg::Progress(p) => {
                if let Some(r) = self.run.as_mut() {
                    r.progress = Some(p);
                }
            }
            Msg::Finished(id, r) => {
                self.run = None;
                match r {
                    Ok(_) => {
                        self.panel = None;
                        self.expanded = Some(id);
                    }
                    Err(e) => self.panel = Some((id, Panel::Failed(e))),
                }
            }
            Msg::OpenLink(url) => {
                if juttusihti_core::feed::is_web_url(&url)
                    && let Err(e) = open::that_detached(&url)
                {
                    tracing::warn!(error = %e, "could not open a fact check source");
                }
            }
            Msg::ToggleCard => {}
        }
        Cmd::none()
    }

    /// The action row's button: "Fact check", "Check again", or the progress of a run.
    pub fn action_button(&self, article: ArticleId, can_check: bool, has_result: bool) -> Element<'_, Msg> {
        if let Some(r) = &self.run {
            if r.article == article {
                let label = match r.progress {
                    Some(FactCheckProgress::Checking { done, total }) => format!("Checking… {done}/{total}"),
                    _ => "Checking…".to_string(),
                };
                return style::quiet_button(None, label, style::quiet, None);
            }
            return tooltip(
                style::quiet_button(None, "Fact check", style::quiet, None),
                style::tip("Another fact check is running."),
                tooltip::Position::Bottom,
            )
            .into();
        }
        let label = if has_result { "Check again" } else { "Fact check" };
        if !can_check {
            return tooltip(
                style::quiet_button(None, label, style::quiet, None),
                style::tip("Load the full article first"),
                tooltip::Position::Bottom,
            )
            .into();
        }
        style::quiet_button(None, label, style::quiet, Some(Msg::Open(article)))
    }

    /// The panel under the action row: consent and estimate, the progress
    /// of a run, or why a check could not run.
    pub fn view_panel(&self, article: ArticleId) -> Option<Element<'_, Msg>> {
        let content: Element<'_, Msg> = match (&self.run, &self.panel) {
            (Some(r), _) if r.article == article => {
                let line = match r.progress {
                    Some(FactCheckProgress::Checking { done, total }) => format!("Checking claims: {done}/{total}"),
                    _ => "Finding the claims…".to_string(),
                };
                row![
                    text(line).size(SIZE_SMALL).style(style::body).width(Fill),
                    style::quiet_button(None, "Cancel", style::quiet, Some(Msg::Cancel)),
                ]
                .spacing(SPACE[1])
                .align_y(Center)
                .into()
            }
            (_, Some((id, panel))) if *id == article => match panel {
                Panel::Planning => style::note("Estimating…").into(),
                Panel::Failed(e) => row![
                    text(e).size(SIZE_SMALL).style(style::danger).width(Fill),
                    style::quiet_button(None, "Close", style::quiet, Some(Msg::Close)),
                ]
                .spacing(SPACE[1])
                .align_y(Center)
                .into(),
                Panel::Ready(plan) => self.view_consent(plan),
            },
            _ => return None,
        };
        Some(container(content).padding(SPACE[3]).width(Fill).style(style::card).into())
    }

    fn view_consent<'a>(&'a self, p: &'a Plan) -> Element<'a, Msg> {
        let small = |line: String| text(line).size(SIZE_SMALL).style(style::body);
        let plan = if p.billing == Billing::ChatGptPlan { " on your ChatGPT plan" } else { "" };
        let mut col = column![
            style::heading("Fact check"),
            style::note(format!("{} · {}{plan}", p.provider, p.model)),
            small(sends_line(p.provider, p.has_google)),
            small(estimate_line(p.billing, &p.estimate)),
        ]
        .spacing(SPACE[1]);
        if !p.web_search {
            col = col
                .push(small("This model has no known web search price, so the check runs without web search.".into()));
        } else if matches!(p.provider, Provider::Openai | Provider::Gemini) {
            col = col.push(small("Web searches can run slightly over the limit.".into()));
        }
        col = col.push(style::note(format!("Limit {}", format_usd(p.limit))));
        if let Err(problem) = &p.ready {
            col = col.push(text(problem).size(SIZE_SMALL).style(style::danger));
        }
        let send = (!self.is_running() && p.can_send()).then_some(Msg::Send);
        col.push(
            row![
                style::main_button(format!("Check with {}", p.provider), send),
                style::quiet_button(None, "Cancel", style::quiet, Some(Msg::Cancel)),
            ]
            .spacing(SPACE[1])
            .align_y(Center),
        )
        .into()
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())
}

/// Works out what a check of `id` would send and cost. Reads only whether a
/// Google Fact Check key is stored; sends nothing.
async fn plan(db: Db, catalog: Catalog, id: ArticleId) -> Result<Box<Plan>, String> {
    let d = ai_setup::defaults(&db).await?;
    let (provider, billing, model) = (d.provider, d.billing(), fact_check_model(&d));
    // The clickbait check plays no part in a fact check, so its key is not needed.
    let mut credentials = d;
    credentials.clickbait.enabled = false;
    let ready = ai_setup::readiness(&credentials).await;
    let has_google = blocking(|| matches!(secrets::google_factcheck_key(), Ok(Some(_)))).await?;
    let (article, text, limit) = db
        .call(move |s| -> juttusihti_core::Result<_> {
            Ok((s.article(id)?, s.article_text(id)?, s.fact_check_limit()?))
        })
        .await?;
    let price = catalog.price_for(provider, &model, billing, Local::now().date_naive());
    let web_search = price.as_ref().is_none_or(|p| p.web_search_per_1k.is_some());
    // Without a search price the check runs without searches: estimate it so.
    let price = price.map(|p| Price { web_search_per_1k: p.web_search_per_1k.or(Some(0)), ..p });
    Ok(Box::new(Plan {
        estimate: estimate(&text, price.as_ref()),
        article: ArticleForCheck { article_id: id, title: article.title, text, url: article.url },
        provider,
        billing,
        model,
        limit,
        has_google,
        web_search,
        ready,
    }))
}

/// Runs the consented check, streaming its progress and then its outcome.
fn run(
    plan: &Plan,
    db: Arc<Mutex<Store>>,
    catalog: Catalog,
    control: Control,
) -> impl iced::futures::Stream<Item = Msg> + use<> {
    let job = FactCheckJob { article: plan.article.clone(), model: plan.model.clone(), limit: plan.limit };
    let (provider, billing, id) = (plan.provider, plan.billing, plan.article.article_id);
    iced::stream::channel(16, async move |mut out: iced::futures::channel::mpsc::Sender<Msg>| {
        use iced::futures::SinkExt;
        let result = check(job, provider, billing, catalog, db, control, &mut out).await;
        let _ = out.send(Msg::Finished(id, result.map(Box::new))).await;
    })
}

async fn check(
    job: FactCheckJob,
    provider: Provider,
    billing: Billing,
    catalog: Catalog,
    db: Arc<Mutex<Store>>,
    control: Control,
    out: &mut iced::futures::channel::mpsc::Sender<Msg>,
) -> Result<FactCheck, String> {
    use iced::futures::SinkExt;
    let endpoint = research::endpoint(provider, billing).await?;
    let client = Client::new(endpoint, RetryPolicy::default()).map_err(|e| e.to_string())?;
    let key = blocking(secrets::google_factcheck_key).await?.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "could not read the Google Fact Check key; checking without it");
        None
    });
    let sources = Sources::new(key)?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = tokio::spawn(async move { run_fact_check(job, &client, &sources, &catalog, db, &control, tx).await });
    while let Some(p) = rx.recv().await {
        let _ = out.send(Msg::Progress(p)).await;
    }
    match handle.await {
        Ok(r) => r.map_err(|e| e.to_string()),
        Err(e) => Err(format!("fact check task failed: {e}")),
    }
}

/// "Sends the article to OpenAI, its claims to Google Fact Check and searches to Wikipedia."
fn sends_line(provider: Provider, has_google: bool) -> String {
    let google = if has_google { ", its claims to Google Fact Check" } else { "" };
    format!("Sends the article to {provider}{google} and searches to Wikipedia.")
}

/// The cost range before sending.
fn estimate_line(billing: Billing, e: &FactCheckEstimate) -> String {
    match (billing, e.expected, e.conservative) {
        (Billing::ChatGptPlan, _, _) => "On your ChatGPT plan".into(),
        (_, Some(_), Some(_)) => format!(
            "About {}, at most about {}",
            cost_text(Some(billing), e.expected),
            cost_text(Some(billing), e.conservative)
        ),
        _ => "Cost unknown".into(),
    }
}

/// The result card: the summary (press to collapse), when and how it was
/// checked, then each claim and guideline.
pub fn view_card<'a>(fc: &'a FactCheck, collapsed: bool) -> Element<'a, Msg> {
    let chevron = if collapsed { Icon::ChevronRight } else { Icon::ChevronDown };
    let summary = row![
        icon(chevron).size(SIZE_SMALL).style(style::muted),
        text(fc.summary_line()).size(SIZE_BODY).font(SEMIBOLD).style(worst_style(fc)),
    ]
    .spacing(SPACE[1])
    .align_y(Center);
    let header = row![
        button(summary).style(style::row).padding([2.0, 0.0]).on_press(Msg::ToggleCard).width(Fill),
        style::quiet_button(None, "Check again", style::quiet, Some(Msg::Open(fc.article_id))),
    ]
    .spacing(SPACE[1])
    .align_y(Center);
    let mut col = column![
        header,
        style::note(format!("Checked {} · {} {}", format::full_date(fc.checked_at), fc.provider, fc.model)),
    ]
    .spacing(SPACE[2]);
    if !fc.web_search {
        col = col.push(style::note("Reduced check: web search was not available."));
    }
    if !collapsed {
        for c in &fc.claims {
            col = col.push(view_claim(c));
        }
        let mut guidelines = column![].spacing(SPACE[1]);
        for g in &fc.guidelines {
            guidelines = guidelines.push(view_guideline(g));
        }
        col = col.push(guidelines);
    }
    col = col.push(text(NOTE).size(SIZE_SMALL).style(style::faint));
    container(col).padding(SPACE[3]).width(Fill).style(style::card).into()
}

/// One claim: its verdict tag and quote, the reason and the source links.
fn view_claim(c: &juttusihti_core::factcheck::ClaimResult) -> Element<'_, Msg> {
    let tag = container(text(c.verdict.label()).size(SIZE_SMALL).style(verdict_style(c.verdict)))
        .padding([2.0, SPACE[1]])
        .style(style::chip);
    let mut links = row![].spacing(SPACE[1]);
    for s in &c.sources {
        links = links.push(
            button(text(source_label(s)).size(SIZE_SMALL).style(style::accent))
                .style(style::row)
                .padding([2.0, SPACE[0]])
                .on_press(Msg::OpenLink(s.url.clone())),
        );
    }
    column![
        row![tag, text(&c.quote).size(SIZE_BODY).style(style::body).width(Fill)].spacing(SPACE[2]),
        text(&c.reason).size(SIZE_SMALL).style(style::muted),
        links.wrap(),
    ]
    .spacing(SPACE[1])
    .into()
}

fn view_guideline(g: &GuidelineCheck) -> Element<'_, Msg> {
    let line = guideline_line(g);
    match g.status {
        GuidelineStatus::Concern => column![
            text(line).size(SIZE_SMALL).style(style::accent),
            text(&g.reason).size(SIZE_SMALL).style(style::muted),
        ]
        .spacing(SPACE[0])
        .into(),
        _ => text(line).size(SIZE_SMALL).style(style::body).into(),
    }
}

/// "JSN 15 Headline backed by text: OK".
fn guideline_line(g: &GuidelineCheck) -> String {
    let status = match g.status {
        GuidelineStatus::Ok => "OK",
        GuidelineStatus::Concern => "Concern",
        GuidelineStatus::NotApplicable => "Not applicable",
    };
    format!("JSN {} {}: {status}", g.guideline.number(), g.guideline.label())
}

fn verdict_style(v: Verdict) -> fn(&iced::Theme) -> text::Style {
    match v {
        Verdict::Contradicted => style::danger,
        Verdict::Misleading | Verdict::Unverified => style::accent,
        Verdict::Supported => style::success,
        Verdict::NotChecked => style::muted,
    }
}

/// The colour of the worst verdict in the check.
fn worst_style(fc: &FactCheck) -> fn(&iced::Theme) -> text::Style {
    let has = |v: Verdict| fc.claims.iter().any(|c| c.verdict == v);
    if has(Verdict::Contradicted) {
        style::danger
    } else if has(Verdict::Misleading) || has(Verdict::Unverified) {
        style::accent
    } else {
        style::success
    }
}

/// A source's link text: "{publisher}: {rating}" for a published fact
/// check, otherwise its title, or its site or domain when the title is empty.
fn source_label(s: &Source) -> String {
    match (s.kind, s.publisher.as_deref(), s.rating.as_deref()) {
        (SourceKind::FactCheck, Some(publisher), Some(rating)) => format!("{publisher}: {rating}"),
        (SourceKind::FactCheck, Some(publisher), None) => publisher.to_string(),
        _ if !s.title.trim().is_empty() => s.title.trim().to_string(),
        _ => match s.site.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            Some(site) => site.to_string(),
            None => url::Url::parse(&s.url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_string))
                .unwrap_or_else(|| s.url.clone()),
        },
    }
}

#[cfg(test)]
mod tests {
    use juttusihti_ai::factcheck::ArticleForCheck;
    use juttusihti_core::ai_defaults::{AiDefaults, Tier, recommended};
    use juttusihti_core::factcheck::estimate;
    use juttusihti_core::pricing::{Billing, Provider};
    use juttusihti_core::store::Store;

    use super::*;
    use crate::db::Db;

    #[test]
    fn can_check_needs_full_text() {
        let body = "x".repeat(400);
        assert!(!can_check(false, None, &body), "teaser only");
        assert!(can_check(true, None, &body), "fetched 400-char body");
        let content = "y".repeat(299);
        assert!(!can_check(false, Some(&content), &content), "feed content of 299 chars");
        assert!(!can_check(false, Some(""), &body), "empty feed content is no content");
    }

    #[test]
    fn fact_check_uses_quality_model() {
        let p = Provider::Anthropic;
        let fast = AiDefaults { provider: p, tier: Tier::Fast, model: Some("my-fast".into()), ..Default::default() };
        assert_eq!(fact_check_model(&fast), recommended(p, Tier::Quality).model);
        let quality = AiDefaults { tier: Tier::Quality, ..fast.clone() };
        assert_eq!(fact_check_model(&quality), "my-fast");
        let plain = AiDefaults { model: None, ..quality };
        assert_eq!(fact_check_model(&plain), recommended(p, Tier::Quality).model);
    }

    fn plan(article: i64) -> Plan {
        let text = "z".repeat(500);
        Plan {
            estimate: estimate(&text, None),
            article: ArticleForCheck { article_id: article, title: "T".into(), text, url: None },
            provider: Provider::Openai,
            billing: Billing::ApiKey,
            model: "gpt-6.1-sol".into(),
            limit: 1_000_000,
            has_google: false,
            web_search: true,
            ready: Ok(()),
        }
    }

    #[test]
    fn consent_panel_sends_nothing_until_send() {
        let mut st = State::new(Db::new(Store::open_in_memory().unwrap()));
        let _ = st.update(Msg::Open(7));
        assert!(!st.is_running());
        let g = st.generation;
        let _ = st.update(Msg::Planned(g, Ok(Box::new(plan(7)))));
        assert!(!st.is_running(), "opening and estimating sends nothing");
        assert!(st.view_panel(7).is_some());
        assert!(st.view_panel(8).is_none());
        let _ = st.update(Msg::Cancel);
        assert!(!st.is_running());
        assert!(st.view_panel(7).is_none(), "cancel closes the panel");
    }

    #[test]
    fn send_needs_ready_credentials_and_a_known_cost() {
        let mut st = State::new(Db::new(Store::open_in_memory().unwrap()));
        let _ = st.update(Msg::Open(7));
        let g = st.generation;
        // Unknown token price on an API key.
        let _ = st.update(Msg::Planned(g, Ok(Box::new(plan(7)))));
        let _ = st.update(Msg::Send);
        assert!(!st.is_running());
        let not_ready = Plan { ready: Err("Add a key".into()), ..plan(7) };
        let _ = st.update(Msg::Planned(g, Ok(Box::new(not_ready))));
        let _ = st.update(Msg::Send);
        assert!(!st.is_running());
        // A stale estimate is dropped.
        let _ = st.update(Msg::Planned(g - 1, Err("old".into())));
        assert!(matches!(st.panel, Some((7, Panel::Ready(_)))));
    }

    #[test]
    fn panel_copy() {
        assert_eq!(sends_line(Provider::Openai, false), "Sends the article to OpenAI and searches to Wikipedia.");
        assert_eq!(
            sends_line(Provider::Gemini, true),
            "Sends the article to Google Gemini, its claims to Google Fact Check and searches to Wikipedia."
        );
        let e = FactCheckEstimate {
            expected: Some(20_000),
            conservative: Some(150_000),
            searches_expected: 8,
            searches_conservative: 24,
            tokens_conservative: 1,
        };
        assert_eq!(estimate_line(Billing::ApiKey, &e), "About $0.0200, at most about $0.1500");
        assert_eq!(estimate_line(Billing::ApiKey, &estimate("x", None)), "Cost unknown");
    }

    #[test]
    fn card_copy() {
        use juttusihti_core::factcheck::Guideline;
        let src = |kind, title: &str, publisher: Option<&str>, rating: Option<&str>| Source {
            url: "https://www.example.org/a".into(),
            title: title.into(),
            kind,
            publisher: publisher.map(Into::into),
            rating: rating.map(Into::into),
            site: None,
        };
        assert_eq!(source_label(&src(SourceKind::Web, "Report", None, None)), "Report");
        assert_eq!(source_label(&src(SourceKind::Web, " ", None, None)), "www.example.org");
        let redirect = Source { site: Some("yle.fi".into()), ..src(SourceKind::Web, "", None, None) };
        assert_eq!(source_label(&redirect), "yle.fi", "a redirect's site, not the redirect host");
        assert_eq!(
            source_label(&src(SourceKind::FactCheck, "t", Some("Faktabaari"), Some("Väärin"))),
            "Faktabaari: Väärin"
        );
        let g = GuidelineCheck {
            guideline: Guideline::Headline15,
            status: GuidelineStatus::NotApplicable,
            reason: String::new(),
        };
        assert_eq!(guideline_line(&g), "JSN 15 Headline backed by text: Not applicable");
    }
}

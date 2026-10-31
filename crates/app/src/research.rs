//! "Custom analysis" screen: scope selection, local preview, cost estimate and
//! saving a session for approval. Nothing here contacts an AI provider.

use std::collections::BTreeSet;

use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone};
use iced::widget::{Space, button, column, container, pick_list, row, rule, scrollable, text, text_input};
use iced::{Center, Element, Fill, Length, Task as Cmd};
use juttusihti_ai::{Approval, Control, Progress, RunSummary};
use juttusihti_core::analysis::{AnalysisConfig, Depth, SessionSummary, Task};
use juttusihti_core::budget::Limits;
use juttusihti_core::estimate::{Estimate, Scenario};
use juttusihti_core::pricing::{Billing, Catalog, Micros, Provider, format_usd, usd_to_micros};
use juttusihti_core::search::SearchQuery;
use juttusihti_core::{Feed, Folder, Label, SessionStatus};

use crate::db::Db;
use crate::format;
use crate::style::{self, SEMIBOLD, SIZE_BODY, SIZE_SMALL, SPACE};

#[derive(Debug, Clone)]
pub struct Lists {
    feeds: Vec<Feed>,
    folders: Vec<Folder>,
    labels: Vec<Label>,
    sessions: Vec<SessionSummary>,
    defaults: Option<juttusihti_core::ai_defaults::AiDefaults>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Result<Lists, String>),
    From(String),
    To(String),
    QuickRange(u32),
    Query(String),
    ToggleFeed(i64, bool),
    ToggleFolder(i64, bool),
    ToggleLabel(i64, bool),
    RequireDate(bool),
    Provider(Provider),
    Billing(Billing),
    LoadPlanModels,
    PlanModels(Result<Vec<String>, String>),
    ModelPicked(String),
    Model(String),
    Depth(Depth),
    ToggleTask(Task, bool),
    Criteria(String),
    MaxArticles(String),
    MaxTokens(String),
    MaxCost(String),
    ReuseCache(bool),
    AcceptUnknownCost(bool),
    Preview,
    Previewed(Result<Box<(AnalysisConfig, Estimate)>, String>),
    Save,
    Saved(Result<i64, String>),
    SelectSession(i64),
    SessionLoaded(Result<Box<SessionDetail>, String>),
    Consent(bool),
    StartRun,
    Run(RunEvent),
    PauseRun,
    CancelRun,
    /// Handled by the app: open the Review screen for a session.
    ReviewSession(i64),
}

/// A saved session as shown in the run panel.
#[derive(Debug, Clone)]
pub struct SessionDetail {
    id: i64,
    status: SessionStatus,
    error: Option<String>,
    config: AnalysisConfig,
    estimate: Option<Estimate>,
    spent: juttusihti_core::budget::Totals,
}

#[derive(Debug, Clone)]
pub enum RunEvent {
    Progress(Progress),
    Finished(Result<RunSummary, String>),
}

struct ActiveRun {
    session: i64,
    control: Control,
    log: Vec<String>,
    tokens: u64,
    cost: Option<u64>,
    billing: Billing,
}

pub struct State {
    db: Db,
    catalog: Catalog,
    lists: Option<Lists>,
    from: String,
    to: String,
    query: String,
    feeds: BTreeSet<i64>,
    folders: BTreeSet<i64>,
    labels: BTreeSet<i64>,
    require_date: bool,
    provider: Provider,
    billing: Billing,
    /// Models the signed-in ChatGPT plan offers, once loaded.
    plan_models: Option<Vec<String>>,
    model: String,
    depth: Depth,
    tasks: BTreeSet<Task>,
    criteria: String,
    max_articles: String,
    max_tokens: String,
    max_cost: String,
    reuse_cache: bool,
    accept_unknown_cost: bool,
    busy: bool,
    error: Option<String>,
    preview: Option<(AnalysisConfig, Estimate)>,
    saved: Option<i64>,
    session: Option<SessionDetail>,
    consent: bool,
    run: Option<ActiveRun>,
    last_log: Vec<String>,
    /// The AI defaults fill the form once, on the first load.
    defaults_applied: bool,
}

const DATE_FORMATS: &[&str] = &["%-d.%-m.%Y", "%d.%m.%Y", "%Y-%m-%d"];

fn parse_date(s: &str) -> Option<NaiveDate> {
    DATE_FORMATS.iter().find_map(|f| NaiveDate::parse_from_str(s.trim(), f).ok())
}

fn show_date(d: NaiveDate) -> String {
    d.format("%-d.%-m.%Y").to_string()
}

/// Local midnight of `day` as UTC seconds. On a DST gap the first valid
/// instant is used.
fn local_midnight(day: NaiveDate) -> Option<i64> {
    let naive = day.and_hms_opt(0, 0, 0)?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .or_else(|| Local.from_local_datetime(&(naive + Duration::hours(1))).earliest())
        .map(|d| d.timestamp())
}

impl State {
    pub fn new(db: Db) -> Self {
        let today = Local::now().date_naive();
        let catalog = Catalog::builtin();
        let provider = Provider::Anthropic;
        let model = catalog.models(provider).into_iter().find(|m| m.contains("haiku")).unwrap_or_default();
        Self {
            db,
            catalog,
            lists: None,
            from: show_date(today - Duration::days(29)),
            to: show_date(today),
            query: String::new(),
            feeds: BTreeSet::new(),
            folders: BTreeSet::new(),
            labels: BTreeSet::new(),
            require_date: false,
            provider,
            billing: Billing::ApiKey,
            plan_models: None,
            model,
            depth: Depth::HeadlinesAndDescriptions,
            tasks: [Task::Labels, Task::Related].into_iter().collect(),
            criteria: String::new(),
            max_articles: "500".into(),
            max_tokens: "2000000".into(),
            max_cost: "1.00".into(),
            reuse_cache: true,
            accept_unknown_cost: false,
            busy: false,
            error: None,
            preview: None,
            saved: None,
            session: None,
            consent: false,
            run: None,
            last_log: Vec::new(),
            defaults_applied: false,
        }
    }

    fn load_session(&self, id: i64) -> Cmd<Msg> {
        let db = self.db.clone();
        Cmd::perform(
            async move {
                db.call(move |s| -> juttusihti_core::Result<Box<SessionDetail>> {
                    let rec = s.session(id)?;
                    let config: AnalysisConfig = serde_json::from_value(rec.config)?;
                    let estimate = rec.estimate.map(serde_json::from_value).transpose()?;
                    Ok(Box::new(SessionDetail {
                        id,
                        status: rec.status,
                        error: rec.error,
                        config,
                        estimate,
                        spent: s.session_usage(id)?,
                    }))
                })
                .await
            },
            Msg::SessionLoaded,
        )
    }

    fn start_run(&mut self) -> Cmd<Msg> {
        let Some(detail) = &self.session else { return Cmd::none() };
        if !self.consent || self.run.is_some() {
            return Cmd::none();
        }
        let (id, provider, billing) = (detail.id, detail.config.provider, detail.config.billing);
        let control = Control::default();
        self.run = Some(ActiveRun {
            session: id,
            control: control.clone(),
            log: Vec::new(),
            tokens: 0,
            cost: Some(0),
            billing,
        });
        Cmd::run(run_session(self.db.shared(), self.catalog.clone(), id, provider, billing, control), Msg::Run)
    }

    pub fn load(&self) -> Cmd<Msg> {
        let db = self.db.clone();
        Cmd::perform(
            async move {
                let defaults = crate::ai_setup::defaults(&db).await.ok();
                db.call(move |s| -> juttusihti_core::Result<Lists> {
                    Ok(Lists {
                        feeds: s.feeds()?,
                        folders: s.folders()?,
                        labels: s.labels()?,
                        sessions: s.recent_sessions(20)?,
                        defaults,
                    })
                })
                .await
            },
            Msg::Loaded,
        )
    }

    /// Validates the form into a configuration.
    fn config(&self) -> Result<AnalysisConfig, String> {
        let from = parse_date(&self.from).ok_or("Start date is not a valid date (use d.m.yyyy).")?;
        let to = parse_date(&self.to).ok_or("End date is not a valid date (use d.m.yyyy).")?;
        if to < from {
            return Err("The end date is before the start date.".into());
        }
        // Inclusive end date in the UI, exclusive bound internally.
        let from_ts = local_midnight(from).ok_or("Start date cannot be represented in local time.")?;
        let to_ts = local_midnight(to + Duration::days(1)).ok_or("End date cannot be represented in local time.")?;
        if self.model.trim().is_empty() {
            return Err("Choose or enter a model.".into());
        }
        let parse_u = |s: &str, what: &str| -> Result<Option<u64>, String> {
            let t = s.trim().replace([' ', '\u{a0}'], "");
            if t.is_empty() {
                Ok(None)
            } else {
                t.parse().map(Some).map_err(|_| format!("{what} must be a whole number."))
            }
        };
        let max_articles = parse_u(&self.max_articles, "Maximum articles")?.unwrap_or(500);
        if max_articles == 0 || max_articles > 100_000 {
            return Err("Maximum articles must be between 1 and 100 000.".into());
        }
        let max_cost = {
            let t = self.max_cost.trim().trim_start_matches('$').replace(',', ".");
            if t.is_empty() {
                None
            } else {
                let v: f64 = t.parse().map_err(|_| "Maximum cost must be a number of dollars.")?;
                Some(usd_to_micros(v).map_err(|e| e.to_string())?)
            }
        };
        let tz = iana_hint();
        Ok(AnalysisConfig {
            scope: SearchQuery {
                text: self.query.trim().to_string(),
                feed_ids: self.feeds.iter().copied().collect(),
                folder_ids: self.folders.iter().copied().collect(),
                label_ids: self.labels.iter().copied().collect(),
                from: Some(from_ts),
                to: Some(to_ts),
                require_published_date: self.require_date,
                ..Default::default()
            },
            range_label: format!("{} – {}{tz}", show_date(from), show_date(to)),
            provider: self.provider,
            billing: if self.provider == Provider::Openai { self.billing } else { Billing::ApiKey },
            model: self.model.trim().to_string(),
            depth: self.depth,
            tasks: self.tasks.clone(),
            limits: Limits {
                max_tokens: parse_u(&self.max_tokens, "Maximum tokens")?,
                max_cost,
                max_calls: None,
                allow_unknown_cost: self.accept_unknown_cost,
            },
            max_articles: max_articles as u32,
            reuse_cached: self.reuse_cache,
            use_batch: false,
            relevance_criteria: (self.tasks.contains(&Task::Relevance) && !self.criteria.trim().is_empty())
                .then(|| self.criteria.trim().to_string()),
        })
    }

    fn invalidate(&mut self) {
        self.preview = None;
        self.saved = None;
    }

    pub fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Loaded(Ok(lists)) => {
                if !self.defaults_applied
                    && let Some(d) = &lists.defaults
                {
                    self.defaults_applied = true;
                    self.provider = d.provider;
                    self.billing = d.billing();
                    self.model = d.model();
                    self.tasks = d.tasks.clone();
                    if let Some(c) = d.max_cost {
                        self.max_cost = format_usd(c).trim_start_matches('$').to_string();
                    }
                    if let Some(t) = d.max_tokens {
                        self.max_tokens = t.to_string();
                    }
                }
                self.lists = Some(lists);
            }
            Msg::Loaded(Err(e)) => self.error = Some(e),
            Msg::From(s) => {
                self.from = s;
                self.invalidate();
            }
            Msg::To(s) => {
                self.to = s;
                self.invalidate();
            }
            Msg::QuickRange(days) => {
                let today = Local::now().date_naive();
                self.from = show_date(range_start(days, today));
                self.to = show_date(today);
                self.invalidate();
            }
            Msg::Query(s) => {
                self.query = s;
                self.invalidate();
            }
            Msg::ToggleFeed(id, on) => {
                toggle(&mut self.feeds, id, on);
                self.invalidate();
            }
            Msg::ToggleFolder(id, on) => {
                toggle(&mut self.folders, id, on);
                self.invalidate();
            }
            Msg::ToggleLabel(id, on) => {
                toggle(&mut self.labels, id, on);
                self.invalidate();
            }
            Msg::RequireDate(on) => {
                self.require_date = on;
                self.invalidate();
            }
            Msg::Provider(p) => {
                self.provider = p;
                if p != Provider::Openai {
                    self.billing = Billing::ApiKey;
                }
                self.model = juttusihti_core::ai_defaults::recommended(p, juttusihti_core::ai_defaults::Tier::Fast)
                    .model
                    .to_string();
                self.invalidate();
            }
            Msg::Billing(b) => {
                self.billing = b;
                self.invalidate();
            }
            Msg::LoadPlanModels => {
                self.busy = true;
                self.error = None;
                return Cmd::perform(crate::plan::models(), Msg::PlanModels);
            }
            Msg::PlanModels(r) => {
                self.busy = false;
                match r {
                    Ok(models) if models.is_empty() => {
                        self.error = Some("Your ChatGPT plan offered no models for Juttusihti.".into())
                    }
                    Ok(models) => {
                        if !models.contains(&self.model) {
                            self.model = models[0].clone();
                            self.invalidate();
                        }
                        self.plan_models = Some(models);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::ModelPicked(m) | Msg::Model(m) => {
                self.model = m;
                self.invalidate();
            }
            Msg::Depth(d) => {
                self.depth = d;
                self.invalidate();
            }
            Msg::ToggleTask(t, on) => {
                if on {
                    self.tasks.insert(t);
                } else {
                    self.tasks.remove(&t);
                }
                self.invalidate();
            }
            Msg::Criteria(s) => {
                self.criteria = s;
                self.invalidate();
            }
            Msg::MaxArticles(s) => {
                self.max_articles = s;
                self.invalidate();
            }
            Msg::MaxTokens(s) => {
                self.max_tokens = s;
                self.invalidate();
            }
            Msg::MaxCost(s) => {
                self.max_cost = s;
                self.invalidate();
            }
            Msg::ReuseCache(on) => {
                self.reuse_cache = on;
                self.invalidate();
            }
            Msg::AcceptUnknownCost(on) => {
                self.accept_unknown_cost = on;
                self.invalidate();
            }
            Msg::Preview => {
                let config = match self.config() {
                    Ok(c) => c,
                    Err(e) => {
                        self.error = Some(e);
                        return Cmd::none();
                    }
                };
                self.error = None;
                self.busy = true;
                let db = self.db.clone();
                let catalog = self.catalog.clone();
                return Cmd::perform(
                    async move {
                        db.call(move |s| -> juttusihti_core::Result<Box<(AnalysisConfig, Estimate)>> {
                            let scope = s.analysis_scope(&config)?;
                            let vocab = s.labels()?.len();
                            let e = juttusihti_core::estimate::estimate(
                                &config,
                                &scope,
                                vocab,
                                &catalog,
                                Local::now().date_naive(),
                            );
                            Ok(Box::new((config, e)))
                        })
                        .await
                    },
                    Msg::Previewed,
                );
            }
            Msg::Previewed(result) => {
                self.busy = false;
                match result {
                    Ok(p) => self.preview = Some(*p),
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::Save => {
                let Some((config, estimate)) = self.preview.clone() else { return Cmd::none() };
                self.busy = true;
                let db = self.db.clone();
                return Cmd::perform(
                    async move {
                        db.call(move |s| {
                            // Rebuild the scope so the snapshot matches the data at save time.
                            let scope = s.analysis_scope(&config)?;
                            s.save_estimated_session(&config, &scope, &estimate)
                        })
                        .await
                    },
                    Msg::Saved,
                );
            }
            Msg::SelectSession(id) => {
                self.consent = false;
                return self.load_session(id);
            }
            Msg::SessionLoaded(Ok(d)) => {
                if self.session.as_ref().is_none_or(|s| s.id != d.id) {
                    self.consent = false;
                }
                self.session = Some(*d);
            }
            Msg::SessionLoaded(Err(e)) => self.error = Some(e),
            Msg::Consent(on) => self.consent = on,
            Msg::StartRun => return self.start_run(),
            Msg::PauseRun => {
                if let Some(r) = &self.run {
                    r.control.pause();
                }
            }
            Msg::CancelRun => {
                if let Some(r) = &self.run {
                    r.control.cancel();
                }
            }
            Msg::ReviewSession(_) => {}
            Msg::Run(RunEvent::Progress(p)) => {
                if let Some(r) = self.run.as_mut() {
                    match p {
                        Progress::Stage { name, requests } => r.log.push(format!("{name}: {requests} requests")),
                        Progress::Call { stage, done, total, tokens, cost } => {
                            r.tokens = tokens;
                            r.cost = cost;
                            r.log.push(format!("{stage}: {done}/{total} done"));
                        }
                        Progress::Note(n) => r.log.push(n),
                    }
                    if r.log.len() > 200 {
                        r.log.drain(..r.log.len() - 200);
                    }
                }
            }
            Msg::Run(RunEvent::Finished(result)) => {
                let session = self.run.as_ref().map(|r| r.session);
                if let Some(r) = self.run.as_mut() {
                    match &result {
                        Ok(s) => r.log.push(format!(
                            "Finished: {} · {} calls · {} tokens · {}",
                            s.final_status.map(|st| st.as_str()).unwrap_or("?"),
                            s.calls,
                            thousands(s.usage.total()),
                            cost_text(Some(r.billing), s.cost)
                        )),
                        Err(e) => r.log.push(format!("Stopped: {e}")),
                    }
                }
                let finished_log = self.run.take().map(|r| r.log).unwrap_or_default();
                self.last_log = finished_log;
                let mut cmds = vec![self.load()];
                if let Some(id) = session {
                    cmds.push(self.load_session(id));
                }
                return Cmd::batch(cmds);
            }
            Msg::Saved(result) => {
                self.busy = false;
                match result {
                    Ok(id) => {
                        self.saved = Some(id);
                        return self.load();
                    }
                    Err(e) => self.error = Some(e),
                }
            }
        }
        Cmd::none()
    }

    /// The quick range ("7 days", "30 days" or "This month") the dates match, if any.
    fn current_range(&self) -> Option<u32> {
        let today = Local::now().date_naive();
        [7, 30, 0]
            .into_iter()
            .find(|&days| self.from == show_date(range_start(days, today)) && self.to == show_date(today))
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let ranges = [(Some(7), "7 days"), (Some(30), "30 days"), (Some(0), "This month")];
        let dates = column![
            row![
                style::heading("Time range"),
                style::help(format!(
                    "Both dates are inclusive, in local time{}. Articles without a publication date use their \
                     fetch time unless excluded.",
                    iana_hint()
                )),
            ]
            .spacing(SPACE[1])
            .align_y(Center),
            row![
                input("From d.m.yyyy", &self.from, Msg::From).width(140),
                text("–").size(SIZE_BODY),
                input("To d.m.yyyy", &self.to, Msg::To).width(140),
                style::segmented(ranges, self.current_range(), |d| Msg::QuickRange(d.unwrap_or(0))),
            ]
            .spacing(SPACE[1])
            .align_y(Center),
            style::check(self.require_date, "Only articles with a publication date").on_toggle(Msg::RequireDate),
        ]
        .spacing(SPACE[1]);

        let mut sources = column![style::heading("Sources (none selected = all)")].spacing(SPACE[1]);
        if let Some(lists) = &self.lists {
            let mut picks = column![].spacing(SPACE[0]);
            for f in &lists.folders {
                picks = picks.push(
                    style::check(self.folders.contains(&f.id), format!("Folder: {}", f.name))
                        .on_toggle(move |on| Msg::ToggleFolder(f.id, on)),
                );
            }
            for f in &lists.feeds {
                let id = f.id;
                picks = picks.push(
                    style::check(self.feeds.contains(&id), f.title.clone())
                        .on_toggle(move |on| Msg::ToggleFeed(id, on)),
                );
            }
            if !lists.labels.is_empty() {
                picks = picks.push(style::note("Only articles with confirmed labels:"));
                for l in &lists.labels {
                    let id = l.id;
                    picks = picks.push(
                        style::check(self.labels.contains(&id), l.name.clone())
                            .on_toggle(move |on| Msg::ToggleLabel(id, on)),
                    );
                }
            }
            sources = sources.push(container(scrollable(picks).height(Length::Shrink)).max_height(180));
        }
        sources = sources.push(
            text_input("Optional search query (same syntax as the search box)", &self.query)
                .on_input(Msg::Query)
                .size(SIZE_BODY)
                .padding(SPACE[1]),
        );

        let on_plan = self.provider == Provider::Openai && self.billing == Billing::ChatGptPlan;
        let models = match (&self.plan_models, on_plan) {
            (Some(m), true) => m.clone(),
            _ => self.catalog.models(self.provider),
        };
        let selected_model = models.iter().find(|m| **m == self.model).cloned();
        let mut provider = column![
            style::heading("AI provider and model"),
            row![
                pick_list(&Provider::ALL[..], Some(self.provider), Msg::Provider).text_size(SIZE_BODY),
                pick_list(models, selected_model, Msg::ModelPicked)
                    .placeholder(if on_plan { "Plan models" } else { "Known models" })
                    .text_size(SIZE_BODY),
                text_input("or any model id", &self.model)
                    .on_input(Msg::Model)
                    .size(SIZE_BODY)
                    .padding(SPACE[1])
                    .width(220),
            ]
            .spacing(SPACE[1])
            .align_y(Center),
        ]
        .spacing(SPACE[1]);
        {
            use juttusihti_core::ai_defaults::{Tier, recommended};
            let fast = recommended(self.provider, Tier::Fast);
            let quality = recommended(self.provider, Tier::Quality);
            provider =
                provider.push(style::note(format!("Recommended — fast: {} · quality: {}", fast.model, quality.model)));
        }
        if self.provider == Provider::Openai {
            provider = provider.push(
                row![
                    style::choice("API key (billed per token)", Billing::ApiKey, Some(self.billing), Msg::Billing),
                    style::choice("My ChatGPT plan", Billing::ChatGptPlan, Some(self.billing), Msg::Billing),
                ]
                .spacing(SPACE[3]),
            );
        }
        if on_plan {
            provider = provider.push(
                row![
                    style::quiet_button(
                        None,
                        "Load my plan's models",
                        style::quiet,
                        (!self.busy).then_some(Msg::LoadPlanModels)
                    ),
                    style::note("Asks OpenAI which models your plan offers. No articles are sent."),
                ]
                .spacing(SPACE[1])
                .align_y(Center),
            );
        }

        let mut depth = column![style::heading("Content to analyze")].spacing(SPACE[1]);
        for d in Depth::ALL {
            let hint = match d {
                Depth::Headlines => " — lowest token usage",
                Depth::HeadlinesAndDescriptions => " — recommended",
                Depth::FullText => " — highest usage; only locally archived text",
            };
            depth = depth.push(style::choice(format!("{}{hint}", d.label()), d, Some(self.depth), Msg::Depth));
        }

        let mut tasks = column![style::heading("Analysis tasks")].spacing(SPACE[1]);
        for t in Task::ALL {
            tasks = tasks
                .push(style::check(self.tasks.contains(&t), t.label()).on_toggle(move |on| Msg::ToggleTask(t, on)));
        }
        if self.tasks.contains(&Task::Relevance) {
            tasks = tasks.push(
                text_input("Investigation topic for relevance, e.g. \"energy prices in the Nordics\"", &self.criteria)
                    .on_input(Msg::Criteria)
                    .size(SIZE_BODY)
                    .padding(SPACE[1]),
            );
        }

        let limits = column![
            style::heading("Limits"),
            row![
                labeled("Max articles", input("500", &self.max_articles, Msg::MaxArticles).width(100.0)),
                labeled("Max tokens", input("2000000", &self.max_tokens, Msg::MaxTokens).width(120.0)),
                labeled("Max cost (USD)", input("1.00", &self.max_cost, Msg::MaxCost).width(100.0)),
            ]
            .spacing(SPACE[3]),
            style::check(self.reuse_cache, "Reuse earlier compatible results").on_toggle(Msg::ReuseCache),
            style::check(self.accept_unknown_cost, "I accept running with unknown pricing")
                .on_toggle(Msg::AcceptUnknownCost),
        ]
        .spacing(SPACE[1]);

        // The main button is "Preview estimate" until a saved session can be started.
        let preview_label = if self.busy { "Working…" } else { "Preview estimate" };
        let preview_press = (!self.busy).then_some(Msg::Preview);
        let mut actions = row![if self.session_startable() {
            style::quiet_button(None, preview_label, style::quiet, preview_press)
        } else {
            style::main_button(preview_label, preview_press)
        }]
        .spacing(SPACE[1])
        .align_y(Center);
        if self.preview.is_some() {
            actions = actions.push(style::quiet_button(
                None,
                "Save session for approval",
                style::quiet,
                (!self.busy && self.saved.is_none()).then_some(Msg::Save),
            ));
        }

        let mut form = column![
            column![
                style::title("Custom analysis"),
                style::intro(
                    "Nothing is sent until you start a run; the estimate is computed locally.",
                    "For a quick run on the list you are reading, use the ✨ AI button above the article list. \
                     AI analysis only runs when you start it. The preview below is computed locally; no data \
                     leaves this computer.",
                ),
            ]
            .spacing(SPACE[1]),
            dates,
            sources,
            provider,
            depth,
            tasks,
            limits,
            actions,
        ]
        .spacing(SPACE[4])
        .padding(SPACE[4])
        .max_width(760);
        if let Some(e) = &self.error {
            form = form.push(text(e).size(SIZE_SMALL).style(style::danger));
        }
        if let Some(id) = self.saved {
            form = form.push(
                text(format!("Saved as session #{id}, awaiting approval. Select it under Sessions to start it."))
                    .size(SIZE_SMALL)
                    .style(style::body),
            );
        }

        let right = column![self.view_estimate(), rule::horizontal(1), self.view_sessions()]
            .spacing(SPACE[4])
            .padding(SPACE[4]);
        row![
            scrollable(form).width(Length::FillPortion(3)),
            rule::vertical(1),
            scrollable(right).width(Length::FillPortion(2))
        ]
        .height(Fill)
        .into()
    }

    /// Whether the selected session shows the "Start analysis"/"Resume" button.
    fn session_startable(&self) -> bool {
        self.session.as_ref().is_some_and(|d| {
            self.run.as_ref().is_none_or(|r| r.session != d.id)
                && matches!(d.status, SessionStatus::AwaitingApproval | SessionStatus::Paused | SessionStatus::Failed)
        })
    }

    fn view_estimate(&self) -> Element<'_, Msg> {
        let Some((config, e)) = &self.preview else {
            return column![
                style::heading("Pre-analysis estimate"),
                style::note(
                    "Press “Preview estimate” to count matching articles, cached results and projected tokens and cost."
                ),
            ]
            .spacing(SPACE[1])
            .into();
        };
        let s = &e.stats;
        let stat = |label: &str, value: String| {
            row![
                text(label.to_string()).size(SIZE_SMALL).style(style::muted).width(220),
                text(value).size(SIZE_SMALL).font(SEMIBOLD)
            ]
        };
        let mut col = column![
            style::heading("Pre-analysis estimate"),
            style::note(format!(
                "{} · {} · {} · {}",
                config.range_label, config.provider, config.model, config.billing
            )),
            stat("Articles matched", s.matched.to_string()),
            stat("Included (max limit)", s.selected.to_string()),
            stat("Distinct after dedup", format!("{} ({} duplicates found locally)", s.distinct, s.local_duplicates)),
            stat("Reusing cached results", s.cached.to_string()),
            stat("Articles requiring AI", s.to_process.to_string()),
            stat("Candidate story pairs", e.candidate_pairs.to_string()),
            stat("Planned requests", e.requests.to_string()),
        ]
        .spacing(SPACE[0]);

        col = col.push(Space::new().height(SPACE[1])).push(style::heading("By stage (expected)"));
        for st in &e.stages {
            col = col.push(style::note(format!(
                "{}: {} requests, {} in / {} out tokens",
                st.stage.label(),
                st.requests,
                thousands(st.expected_input),
                thousands(st.expected_output)
            )));
        }

        col = col.push(Space::new().height(SPACE[1])).push(style::heading("Tokens and cost"));
        for (sc, name) in [
            (Scenario::Optimistic, "Optimistic"),
            (Scenario::Expected, "Expected"),
            (Scenario::Conservative, "Conservative"),
        ] {
            let t = &e.scenarios[&sc];
            let cost = cost_text(Some(config.billing), t.cost);
            let cell = |s: String| text(s).size(SIZE_SMALL).style(style::body);
            col = col.push(
                row![
                    cell(name.into()).width(110),
                    cell(format!("{} in", thousands(t.usage.prompt_tokens()))).width(120),
                    cell(format!("{} out", thousands(t.usage.output))).width(110),
                    text(cost).size(SIZE_SMALL).font(SEMIBOLD),
                ]
                .spacing(SPACE[1]),
            );
        }
        col = col.push(style::note(format!(
            "Worst case if every call uses its full output limit: {} tokens",
            thousands(e.worst_case_tokens)
        )));
        let limits = &config.limits;
        col = col.push(style::note(format!(
            "Limits: {} tokens, {}",
            limits.max_tokens.map(thousands).unwrap_or_else(|| "no token limit".into()),
            limits.max_cost.map(format_usd).unwrap_or_else(|| "no cost limit".into())
        )));
        col = col.push(
            text(match &e.price_source {
                Some(src) => {
                    format!("Prices: {src}. Estimates are not invoices; token counts are local approximations.")
                }
                None => "No price on record for this model.".into(),
            })
            .size(SIZE_SMALL)
            .style(style::faint),
        );
        for w in &e.warnings {
            col = col.push(
                container(text(format!("⚠ {w}")).size(SIZE_SMALL).style(style::body))
                    .padding(SPACE[1])
                    .style(style::card),
            );
        }
        col.into()
    }

    fn view_sessions(&self) -> Element<'_, Msg> {
        let mut col = column![style::heading("Sessions")].spacing(SPACE[0]);
        match &self.lists {
            Some(l) if !l.sessions.is_empty() => {
                for s in &l.sessions {
                    let selected = self.session.as_ref().is_some_and(|d| d.id == s.id);
                    col = col.push(
                        button(
                            text(format!(
                                "#{} · {} · {} · {} · {}",
                                s.id,
                                format::full_date(s.created_at),
                                s.status.as_str(),
                                s.range_label,
                                s.model
                            ))
                            .size(SIZE_SMALL),
                        )
                        .width(Fill)
                        .padding([SPACE[0], SPACE[1]])
                        .style(if selected { style::row_selected } else { style::row })
                        .on_press(Msg::SelectSession(s.id)),
                    );
                }
            }
            _ => col = col.push(style::note("No sessions yet.")),
        }
        if let Some(d) = &self.session {
            col = col.push(Space::new().height(SPACE[1])).push(self.view_session(d));
        }
        col.into()
    }

    fn view_session<'a>(&'a self, d: &'a SessionDetail) -> Element<'a, Msg> {
        let c = &d.config;
        let mut col = column![
            style::heading(format!("Session #{} — {}", d.id, d.status.as_str())),
            style::note(format!("{} · {} · {} · {}", c.range_label, c.provider, c.model, c.billing)),
            style::note(format!(
                "Content sent: {}. Tasks: {}.",
                c.depth,
                c.tasks.iter().map(|t| t.label()).collect::<Vec<_>>().join(", ")
            )),
        ]
        .spacing(SPACE[1]);
        if let Some(e) = &d.estimate {
            col = col.push(style::note(format!(
                "Estimate: {} articles, {} requests, expected {} (conservative {}).",
                e.stats.to_process,
                e.requests,
                cost_text(Some(c.billing), e.expected().cost),
                cost_text(Some(c.billing), e.conservative().cost)
            )));
        }
        if d.spent.calls > 0 {
            col = col.push(style::note(format!(
                "Already used: {} calls, {} tokens, {}{}.",
                d.spent.calls,
                thousands(d.spent.tokens),
                cost_text(Some(c.billing), Some(d.spent.cost)),
                if d.spent.cost_unknown { " + unknown" } else { "" }
            )));
        }
        if let Some(e) = &d.error {
            col = col.push(text(e).size(SIZE_SMALL).style(style::danger));
        }
        let running_here = self.run.as_ref().is_some_and(|r| r.session == d.id);
        if running_here {
            let r = self.run.as_ref().expect("checked");
            col = col.push(
                text(format!("Running… {} tokens so far, {}", thousands(r.tokens), cost_text(Some(c.billing), r.cost)))
                    .size(SIZE_SMALL)
                    .font(SEMIBOLD),
            );
            col = col.push(
                row![
                    style::quiet_button(None, "Pause", style::quiet, Some(Msg::PauseRun)),
                    style::quiet_button(None, "Cancel", style::quiet_danger, Some(Msg::CancelRun)),
                ]
                .spacing(SPACE[1]),
            );
            col = col.push(
                text("Cancelling stops new requests at once. A request already accepted by the provider may still be charged.")
                    .size(SIZE_SMALL)
                    .style(style::faint),
            );
        } else if self.session_startable() {
            col = col.push(
                style::check(
                    self.consent,
                    format!(
                        "I agree to send the content of these articles ({}) to {}{}.",
                        c.depth.label().to_lowercase(),
                        c.provider,
                        if c.billing == Billing::ChatGptPlan { ", using my ChatGPT plan" } else { "" }
                    ),
                )
                .on_toggle(Msg::Consent),
            );
            let label = if d.status == SessionStatus::AwaitingApproval { "Start analysis" } else { "Resume" };
            col = col.push(style::main_button(label, (self.consent && self.run.is_none()).then_some(Msg::StartRun)));
            col = col.push(
                text("Limits are enforced by Juttusihti before each request; they are estimates, not provider billing caps.")
                    .size(SIZE_SMALL)
                    .style(style::faint),
            );
        }
        if !running_here && d.spent.calls > 0 {
            col =
                col.push(style::quiet_button(None, "Review suggestions", style::quiet, Some(Msg::ReviewSession(d.id))));
        }
        let log = self.run.as_ref().filter(|r| r.session == d.id).map(|r| &r.log).unwrap_or(&self.last_log);
        for line in log.iter().rev().take(12) {
            col = col.push(text(line).size(SIZE_SMALL).style(style::faint));
        }
        col.into()
    }
}

/// Runs an approved session with the stored credentials for its billing,
/// streaming progress. The caller has already obtained the user's consent.
pub fn run_session(
    shared: std::sync::Arc<std::sync::Mutex<juttusihti_core::store::Store>>,
    catalog: Catalog,
    id: i64,
    provider: Provider,
    billing: Billing,
    control: Control,
) -> impl iced::futures::Stream<Item = RunEvent> {
    iced::stream::channel(64, async move |mut out: iced::futures::channel::mpsc::Sender<RunEvent>| {
        use iced::futures::SinkExt;
        let endpoint = match billing {
            Billing::ChatGptPlan => crate::plan::session().await.map(juttusihti_ai::Endpoint::chatgpt),
            Billing::ApiKey => match tokio::task::spawn_blocking(move || crate::secrets::get(provider)).await {
                Ok(Ok(Some(k))) => Ok(juttusihti_ai::Endpoint::official(provider, k)),
                Ok(Ok(None)) => Err(format!("No API key for {provider}. Add one under AI setup.")),
                Ok(Err(e)) => Err(e),
                Err(e) => Err(e.to_string()),
            },
        };
        let client = match endpoint.and_then(|e| {
            juttusihti_ai::Client::new(e, juttusihti_ai::RetryPolicy::default()).map_err(|e| e.to_string())
        }) {
            Ok(c) => c,
            Err(e) => {
                let _ = out.send(RunEvent::Finished(Err(e))).await;
                return;
            }
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let runner = juttusihti_ai::Runner::new(shared, client, catalog, control).with_progress(tx);
        let handle = tokio::spawn(async move { runner.run(id, Approval { consent_to_send: true }).await });
        while let Some(p) = rx.recv().await {
            let _ = out.send(RunEvent::Progress(p)).await;
        }
        let result = match handle.await {
            Ok(Ok(summary)) => Ok(summary),
            Ok(Err(e)) => Err(e.to_string()),
            Err(e) => Err(format!("analysis task failed: {e}")),
        };
        let _ = out.send(RunEvent::Finished(result)).await;
    })
}

/// Dollar cost, or the plan for runs charged to a ChatGPT plan.
pub fn cost_text(billing: Option<Billing>, cost: Option<Micros>) -> String {
    match (billing, cost) {
        (Some(Billing::ChatGptPlan), _) => "on your ChatGPT plan".into(),
        (_, Some(c)) => format_usd(c),
        (_, None) => "cost unknown".into(),
    }
}

fn labeled<'a>(label: &'a str, field: impl Into<Element<'a, Msg>>) -> Element<'a, Msg> {
    column![style::note(label), field.into()].spacing(SPACE[0]).into()
}

/// A text field in the screen's size.
fn input<'a>(placeholder: &'a str, value: &'a str, on: fn(String) -> Msg) -> iced::widget::TextInput<'a, Msg> {
    text_input(placeholder, value).on_input(on).size(SIZE_BODY).padding(SPACE[1])
}

/// First day of a quick range ending `today`: the last `days` days, or this month for 0.
fn range_start(days: u32, today: NaiveDate) -> NaiveDate {
    if days == 0 { today.with_day(1).unwrap_or(today) } else { today - Duration::days(i64::from(days) - 1) }
}

fn toggle(set: &mut BTreeSet<i64>, id: i64, on: bool) {
    if on {
        set.insert(id);
    } else {
        set.remove(&id);
    }
}

pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

/// " (UTC+03:00)" style hint for the current local offset.
fn iana_hint() -> String {
    format!(" (UTC{})", Local::now().format("%:z"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_finnish_and_iso_dates() {
        assert_eq!(parse_date("1.10.2026"), NaiveDate::from_ymd_opt(2026, 10, 1));
        assert_eq!(parse_date("01.10.2026"), NaiveDate::from_ymd_opt(2026, 10, 1));
        assert_eq!(parse_date("2026-10-01"), NaiveDate::from_ymd_opt(2026, 10, 1));
        assert_eq!(parse_date("31.2.2026"), None);
    }

    #[test]
    fn quick_range_is_highlighted_until_the_dates_change() {
        let mut st = State::new(Db::new(juttusihti_core::store::Store::open_in_memory().unwrap()));
        let _ = st.update(Msg::QuickRange(30));
        assert_eq!(st.current_range(), Some(30));
        let _ = st.update(Msg::From("1.1.2020".into()));
        assert_eq!(st.current_range(), None);
    }

    #[test]
    fn groups_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1234567), "1\u{202f}234\u{202f}567");
    }

    #[test]
    fn local_midnights_are_a_day_apart_or_dst_adjusted() {
        let d = NaiveDate::from_ymd_opt(2026, 10, 1).unwrap();
        let a = local_midnight(d).unwrap();
        let b = local_midnight(d.succ_opt().unwrap()).unwrap();
        assert!((b - a - 86_400).abs() <= 3_600);
    }
}

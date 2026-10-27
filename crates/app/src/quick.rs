//! Quick AI: analyze the article list being viewed with one click. The
//! provider, model and limits come from the AI defaults; the estimate is
//! computed locally while the panel is open; the send button names the
//! provider and is the user's consent for that run.

use std::collections::BTreeSet;

use chrono::Local;
use iced::widget::{button, checkbox, column, container, row, text};
use iced::{Element, Fill, Font, Task as Cmd, font};
use juttusihti_ai::{Control, Progress};
use juttusihti_core::ai_defaults::{AiDefaults, task_hint};
use juttusihti_core::analysis::{AnalysisConfig, Task};
use juttusihti_core::estimate::Estimate;
use juttusihti_core::pricing::Catalog;
use juttusihti_core::search::SearchQuery;

use crate::ai_setup;
use crate::db::Db;
use crate::research::{RunEvent, cost_text, run_session, thousands};

/// Tasks offered in the panel; relevance needs a topic and lives in the
/// custom analysis.
const QUICK_TASKS: [Task; 5] = [Task::Headlines, Task::Labels, Task::Related, Task::Entities, Task::Summary];

/// The defaults in effect and whether their credentials are set up.
type Setup = Box<(AiDefaults, Result<(), String>)>;

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Result<Setup, String>),
    ToggleTask(Task, bool),
    Estimated(u64, Result<Box<(AnalysisConfig, Estimate)>, String>),
    Start,
    Saved(Result<i64, String>),
    Run(RunEvent),
    Pause,
    Cancel,
    Close,
    /// Handled by the app: open the AI setup screen.
    OpenSetup,
    /// Handled by the app: open the Review screen for a session.
    Review(i64),
}

struct Running {
    session: i64,
    control: Control,
    stage: String,
    done: usize,
    total: usize,
    tokens: u64,
    cost: Option<u64>,
}

pub struct State {
    db: Db,
    catalog: Catalog,
    pub open: bool,
    scope: SearchQuery,
    scope_label: String,
    defaults: Option<AiDefaults>,
    ready: Option<Result<(), String>>,
    tasks: BTreeSet<Task>,
    generation: u64,
    estimate: Option<(AnalysisConfig, Estimate)>,
    run: Option<Running>,
    /// Outcome of the last run (or an error), and its session.
    result: Option<(Result<String, String>, Option<i64>)>,
}

impl State {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            catalog: Catalog::builtin(),
            open: false,
            scope: SearchQuery::default(),
            scope_label: String::new(),
            defaults: None,
            ready: None,
            tasks: BTreeSet::new(),
            generation: 0,
            estimate: None,
            run: None,
            result: None,
        }
    }

    pub fn is_running(&self) -> bool {
        self.run.is_some()
    }

    /// Whether the panel is about `scope` already.
    pub fn targets(&self, scope: &SearchQuery) -> bool {
        self.scope == *scope
    }

    /// Opens the panel for the list being viewed (or retargets it), with
    /// fresh defaults and estimate.
    pub fn open(&mut self, scope: SearchQuery, label: String) -> Cmd<Msg> {
        self.open = true;
        if self.run.is_none() && (self.scope != scope || self.scope_label != label) {
            self.scope = scope;
            self.scope_label = label;
            self.result = None;
        }
        let db = self.db.clone();
        Cmd::perform(
            async move {
                let d = ai_setup::defaults(&db).await?;
                let ready = ai_setup::readiness(&d).await;
                Ok(Box::new((d, ready)))
            },
            Msg::Loaded,
        )
    }

    fn estimate(&mut self) -> Cmd<Msg> {
        let Some(d) = &self.defaults else { return Cmd::none() };
        self.generation += 1;
        let generation = self.generation;
        let config = d.quick_config(self.scope.clone(), &self.scope_label, &self.tasks);
        let (db, catalog) = (self.db.clone(), self.catalog.clone());
        Cmd::perform(
            async move {
                db.call(move |s| -> juttusihti_core::Result<_> {
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
            move |r| Msg::Estimated(generation, r),
        )
    }

    pub fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Loaded(Ok(b)) => {
                let (d, ready) = *b;
                if self.tasks.is_empty() {
                    self.tasks = d.tasks.clone();
                }
                self.defaults = Some(d);
                self.ready = Some(ready);
                return self.estimate();
            }
            Msg::Loaded(Err(e)) => self.result = Some((Err(e), None)),
            Msg::ToggleTask(t, on) => {
                if on {
                    self.tasks.insert(t);
                } else {
                    self.tasks.remove(&t);
                }
                return self.estimate();
            }
            Msg::Estimated(g, r) if g == self.generation => match r {
                Ok(b) => self.estimate = Some(*b),
                Err(e) => self.result = Some((Err(e), None)),
            },
            Msg::Estimated(..) => {}
            Msg::Start => {
                let (Some((config, estimate)), Some(d)) = (self.estimate.clone(), self.defaults.clone()) else {
                    return Cmd::none();
                };
                if self.run.is_some() || !matches!(self.ready, Some(Ok(()))) {
                    return Cmd::none();
                }
                self.result = None;
                // Remember the chosen tasks for next time.
                let remembered = AiDefaults { tasks: self.tasks.clone(), ..d };
                let db = self.db.clone();
                return Cmd::perform(
                    async move {
                        db.call(move |s| -> juttusihti_core::Result<i64> {
                            s.set_ai_defaults(&remembered)?;
                            // Rebuild the scope so the snapshot matches the data now.
                            let scope = s.analysis_scope(&config)?;
                            s.save_estimated_session(&config, &scope, &estimate)
                        })
                        .await
                    },
                    Msg::Saved,
                );
            }
            Msg::Saved(Ok(id)) => {
                let Some((config, _)) = &self.estimate else { return Cmd::none() };
                let control = Control::default();
                self.run = Some(Running {
                    session: id,
                    control: control.clone(),
                    stage: "Starting".into(),
                    done: 0,
                    total: 0,
                    tokens: 0,
                    cost: Some(0),
                });
                let stream =
                    run_session(self.db.shared(), self.catalog.clone(), id, config.provider, config.billing, control);
                return Cmd::run(stream, Msg::Run);
            }
            Msg::Saved(Err(e)) => self.result = Some((Err(e), None)),
            Msg::Run(RunEvent::Progress(p)) => {
                if let Some(r) = self.run.as_mut() {
                    match p {
                        Progress::Stage { name, requests } => {
                            r.stage = name.to_string();
                            r.done = 0;
                            r.total = requests;
                        }
                        Progress::Call { done, total, tokens, cost, .. } => {
                            (r.done, r.total, r.tokens, r.cost) = (done, total, tokens, cost);
                        }
                        Progress::Note(_) => {}
                    }
                }
            }
            Msg::Run(RunEvent::Finished(result)) => {
                let session = self.run.take().map(|r| r.session);
                let billing = self.estimate.as_ref().map(|(c, _)| c.billing);
                self.result = Some((
                    result.map(|s| {
                        let status = s.final_status.map(|st| st.as_str()).unwrap_or("finished");
                        let what =
                            if status == "completed" { "Done".to_string() } else { format!("Stopped ({status})") };
                        format!(
                            "{what}: {} results from {} requests, {} tokens, {}.",
                            s.suggestions,
                            s.calls,
                            thousands(s.usage.total()),
                            cost_text(billing, s.cost)
                        )
                    }),
                    session,
                ));
                // Articles are now cached for these tasks: refresh the numbers.
                return self.estimate();
            }
            Msg::Pause => {
                if let Some(r) = &self.run {
                    r.control.pause();
                }
            }
            Msg::Cancel => {
                if let Some(r) = &self.run {
                    r.control.cancel();
                }
            }
            Msg::Close => {
                if self.run.is_none() {
                    self.open = false;
                }
            }
            Msg::OpenSetup | Msg::Review(_) => {}
        }
        Cmd::none()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let bold = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
        let mut col = column![
            row![
                text(format!("AI for: {}", self.scope_label)).size(13).font(bold).width(Fill),
                button(text("Setup").size(12)).style(button::text).on_press(Msg::OpenSetup),
                button(text("Close").size(12))
                    .style(button::text)
                    .on_press_maybe(self.run.is_none().then_some(Msg::Close)),
            ]
            .spacing(6)
            .align_y(iced::Center),
        ]
        .spacing(6);

        let mut tasks = row![].spacing(12);
        for t in QUICK_TASKS {
            let label = match t {
                Task::Headlines => "Fix clickbait headlines",
                Task::Labels => "Labels",
                Task::Related => "Related stories",
                Task::Entities => "People & places",
                _ => "Summary",
            };
            tasks = tasks.push(iced::widget::tooltip(
                checkbox(self.tasks.contains(&t))
                    .label(label)
                    .size(14)
                    .text_size(12)
                    .on_toggle_maybe(self.run.is_none().then_some(move |on| Msg::ToggleTask(t, on))),
                container(text(task_hint(t)).size(11)).padding(4).style(container::rounded_box),
                iced::widget::tooltip::Position::Bottom,
            ));
        }
        col = col.push(tasks);

        if let Some(d) = &self.defaults {
            col = col.push(text(format!("Model: {}", ai_setup::describe(d))).size(11));
        }

        if let Some(r) = &self.run {
            col = col.push(
                row![
                    text(format!(
                        "{}: {}/{} · {} tokens · {}",
                        r.stage,
                        r.done,
                        r.total,
                        thousands(r.tokens),
                        cost_text(self.estimate.as_ref().map(|(c, _)| c.billing), r.cost)
                    ))
                    .size(12)
                    .width(Fill),
                    button(text("Pause").size(12)).style(button::secondary).on_press(Msg::Pause),
                    button(text("Cancel").size(12)).style(button::danger).on_press(Msg::Cancel),
                ]
                .spacing(6)
                .align_y(iced::Center),
            );
        } else {
            col = col.push(self.view_send());
        }

        match &self.result {
            Some((Ok(m), session)) => {
                let mut r = row![text(m).size(12).width(Fill)].spacing(6).align_y(iced::Center);
                if let Some(id) = session {
                    r = r.push(button(text("Review").size(12)).style(button::text).on_press(Msg::Review(*id)));
                }
                col = col.push(container(r).padding(6).style(container::success));
            }
            Some((Err(e), _)) => col = col.push(container(text(e).size(12)).padding(6).style(container::danger)),
            None => {}
        }
        container(col).padding(10).width(Fill).style(container::bordered_box).into()
    }

    fn view_send(&self) -> Element<'_, Msg> {
        if let Some(Err(problem)) = &self.ready {
            return row![
                text(problem).size(12).width(Fill),
                button(text("Open AI setup").size(12)).on_press(Msg::OpenSetup),
            ]
            .spacing(6)
            .align_y(iced::Center)
            .into();
        }
        let Some((config, e)) = &self.estimate else { return text("Estimating…").size(12).into() };
        if self.tasks.is_empty() {
            return text("Choose what the AI should do.").size(12).into();
        }
        let s = &e.stats;
        if s.to_process == 0 {
            let msg = if s.selected == 0 {
                "No articles in this list.".to_string()
            } else {
                format!("All {} articles here are already analyzed for these tasks.", s.selected)
            };
            return text(msg).size(12).into();
        }
        let expected = e.expected();
        let cost = cost_text(Some(config.billing), expected.cost);
        let over_limit = e.warnings.iter().find(|w| w.contains("limit") || w.contains("No price"));
        let mut col = column![
            text(format!(
                "{} new of the newest {} articles · {} requests · about {} tokens · {}",
                s.to_process,
                s.selected,
                e.requests,
                thousands(expected.usage.total()),
                cost
            ))
            .size(12),
            button(text(format!("Send {} articles to {}", s.to_process, config.provider)).size(13))
                .on_press_maybe((matches!(self.ready, Some(Ok(()))) && e.price_known).then_some(Msg::Start)),
        ]
        .spacing(6);
        if let Some(w) = over_limit {
            col = col.push(text(format!("⚠ {w}")).size(11));
        }
        col = col.push(
            text("Sends the headline and description of each article. Results are suggestions you can undo.").size(11),
        );
        col.into()
    }
}

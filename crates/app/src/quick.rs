//! Quick AI: analyze the article list being viewed with one click. The
//! provider, model and limits come from the AI defaults; the estimate is
//! computed locally while the panel is open; the send button names the
//! provider and is the user's consent for that run.

use std::collections::BTreeSet;

use chrono::Local;
use iced::widget::{button, checkbox, column, container, row, text, tooltip};
use iced::{Center, Element, Fill, Task as Cmd};
use juttusihti_ai::{Control, Progress};
use juttusihti_core::ai_defaults::{AiDefaults, task_hint};
use juttusihti_core::analysis::{AnalysisConfig, Task};
use juttusihti_core::estimate::Estimate;
use juttusihti_core::pricing::Catalog;
use juttusihti_core::search::SearchQuery;

use crate::ai_setup;
use crate::db::Db;
use crate::icons::{Icon, icon};
use crate::research::{RunEvent, cost_text, run_session, thousands};
use crate::style::{self, SIZE_BODY, SIZE_SMALL, SPACE};

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
    /// A run finished while the popover was closed and the app has not shown its result yet.
    finished_unseen: bool,
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
            finished_unseen: false,
        }
    }

    /// "done/total" of the stage being run, once that stage knows how many requests it makes.
    pub fn badge(&self) -> Option<String> {
        self.run.as_ref().filter(|r| r.total > 0).map(|r| format!("{}/{}", r.done, r.total))
    }

    /// The result line and session of a run that finished while the popover was closed, once.
    pub fn take_finished(&mut self) -> Option<(String, Option<i64>, bool)> {
        if !std::mem::take(&mut self.finished_unseen) {
            return None;
        }
        self.result.as_ref().map(|(r, session)| match r {
            Ok(line) => (line.clone(), *session, false),
            Err(e) => (e.clone(), *session, true),
        })
    }

    /// Pretends a run of `session` has started, for tests outside this module.
    #[cfg(test)]
    pub fn start_test_run(&mut self, session: i64) {
        self.run = Some(Running {
            session,
            control: Control::default(),
            stage: "Labels".into(),
            done: 0,
            total: 0,
            tokens: 0,
            cost: Some(0),
        });
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
                let stream = run_session(
                    self.db.shared(),
                    self.catalog.clone(),
                    id,
                    config.provider,
                    config.billing,
                    config.check_on_api_credit(),
                    control,
                );
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
                        // The reason a run stopped, as a sentence, and where to resume it.
                        let why = s.stop_reason.as_deref().map(str::trim).filter(|w| !w.is_empty()).map(|w| {
                            let end = if w.ends_with('.') { "" } else { "." };
                            let resume = if status == "paused" { " Resume it under Custom analysis." } else { "" };
                            format!("{w}{end}{resume} So far")
                        });
                        let what = match (status, why) {
                            ("completed", _) => "Done".to_string(),
                            (_, Some(why)) => format!("Stopped ({status}). {why}"),
                            (_, None) => format!("Stopped ({status})"),
                        };
                        let mut line = format!(
                            "{what}: {} results from {} requests, {} tokens, {}.",
                            s.suggestions,
                            s.calls,
                            thousands(s.usage.total()),
                            cost_text(billing, s.cost)
                        );
                        let h = &s.headlines;
                        if h.considered > 0 {
                            line.push_str(&format!(
                                " {} of {} headlines looked like clickbait; {} kept as they are.",
                                h.clickbait, h.considered, h.rewrites_avoided
                            ));
                        }
                        line
                    }),
                    session,
                ));
                self.finished_unseen = !self.open;
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
            // Closing never stops a run; the ✨ button keeps showing its progress.
            Msg::Close => self.open = false,
            Msg::OpenSetup | Msg::Review(_) => {}
        }
        Cmd::none()
    }

    /// The popover: title, tasks, model and estimate, send, then progress or the last result.
    pub fn view(&self) -> Element<'_, Msg> {
        let idle = self.run.is_none();
        let header = row![
            style::title(format!("AI for: {}", self.scope_label)).width(Fill),
            quiet("Setup", Msg::OpenSetup),
            button(icon(Icon::Close)).padding(SPACE[0]).style(style::icon).on_press(Msg::Close),
        ]
        .spacing(SPACE[1])
        .align_y(Center);

        let mut tasks = column![].spacing(SPACE[1]);
        for t in QUICK_TASKS {
            let label = match t {
                Task::Headlines => "Fix clickbait headlines",
                Task::Labels => "Labels",
                Task::Related => "Related stories",
                Task::Entities => "People & places",
                _ => "Summary",
            };
            tasks = tasks.push(tooltip(
                checkbox(self.tasks.contains(&t))
                    .label(label)
                    .size(SIZE_BODY)
                    .text_size(SIZE_BODY)
                    .style(style::checkbox)
                    .on_toggle_maybe(idle.then_some(move |on| Msg::ToggleTask(t, on))),
                style::tip(task_hint(t)),
                tooltip::Position::Right,
            ));
        }

        let mut col = column![header, tasks].spacing(SPACE[2]);
        if let Some(d) = &self.defaults {
            col = col.push(small(format!("Model: {}", ai_setup::describe(d))));
        }
        col = col.push(self.view_send());

        if let Some(r) = &self.run {
            let progress = format!(
                "{}: {}/{} · {} tokens · {}",
                r.stage,
                r.done,
                r.total,
                thousands(r.tokens),
                cost_text(self.estimate.as_ref().map(|(c, _)| c.billing), r.cost)
            );
            col = col.push(
                row![
                    text(progress).size(SIZE_SMALL).style(style::body).width(Fill),
                    quiet("Pause", Msg::Pause),
                    quiet("Cancel", Msg::Cancel),
                ]
                .spacing(SPACE[1])
                .align_y(Center),
            );
        }

        match &self.result {
            Some((Ok(m), session)) => {
                let mut line =
                    row![text(m).size(SIZE_SMALL).style(style::body).width(Fill)].spacing(SPACE[1]).align_y(Center);
                if let Some(id) = session {
                    line = line.push(quiet("Review", Msg::Review(*id)));
                }
                col = col.push(line);
            }
            Some((Err(e), _)) => col = col.push(text(e).size(SIZE_SMALL).style(style::danger)),
            None => {}
        }
        container(col).padding(SPACE[3]).width(WIDTH).style(style::popover).into()
    }

    /// The estimate line, the send button and the note on what is sent; or why there is nothing to send.
    fn view_send(&self) -> Element<'_, Msg> {
        if let Some(Err(problem)) = &self.ready {
            return column![
                text(problem).size(SIZE_SMALL).style(style::body),
                style::main_button("Open AI setup", Some(Msg::OpenSetup)),
            ]
            .spacing(SPACE[1])
            .into();
        }
        let Some((config, e)) = &self.estimate else { return small("Estimating…") };
        if self.tasks.is_empty() {
            return small("Choose what the AI should do.");
        }
        let s = &e.stats;
        if s.to_process == 0 {
            let msg = if s.selected == 0 {
                "No articles in this list.".to_string()
            } else {
                format!("All {} articles here are already analyzed for these tasks.", s.selected)
            };
            return small(msg);
        }
        let expected = e.expected();
        let cost = cost_text(Some(config.billing), expected.cost);
        let over_limit = e.warnings.iter().find(|w| w.contains("limit") || w.contains("No price"));
        let can_send = self.run.is_none() && matches!(self.ready, Some(Ok(()))) && e.price_known;
        let mut col = column![small(format!(
            "{} new of the newest {} articles · {} requests · about {} tokens · {}",
            s.to_process,
            s.selected,
            e.requests,
            thousands(expected.usage.total()),
            cost
        ))]
        .spacing(SPACE[2]);
        if let Some(w) = over_limit {
            col = col.push(small(format!("⚠ {w}")));
        }
        col.push(style::main_button(
            format!("Send {} articles to {}", s.to_process, config.provider),
            can_send.then_some(Msg::Start),
        ))
        .push(
            text("Sends the headline and description of each article. Results are suggestions you can undo.")
                .size(SIZE_SMALL)
                .style(style::faint),
        )
        .into()
    }
}

/// Popover width.
pub const WIDTH: f32 = 340.0;

/// A line of secondary text.
fn small<'a>(line: impl text::IntoFragment<'a>) -> Element<'a, Msg> {
    text(line).size(SIZE_SMALL).style(style::muted).into()
}

fn quiet(label: &str, msg: Msg) -> Element<'_, Msg> {
    style::quiet_button(None, label, style::quiet, Some(msg))
}

#[cfg(test)]
mod tests {
    use juttusihti_ai::RunSummary;
    use juttusihti_core::store::Store;

    use super::*;

    fn test_db() -> Db {
        Db::new(Store::open_in_memory().unwrap())
    }

    fn running(done: usize, total: usize) -> Running {
        Running {
            session: 1,
            control: Control::default(),
            stage: "Labels".into(),
            done,
            total,
            tokens: 0,
            cost: Some(0),
        }
    }

    /// Ends the current run as session `session`.
    fn finish(st: &mut State, session: i64) {
        if let Some(r) = st.run.as_mut() {
            r.session = session;
        }
        let _ = st.update(Msg::Run(RunEvent::Finished(Ok(RunSummary::default()))));
    }

    #[test]
    fn closing_mid_run_keeps_the_run() {
        let mut st = State::new(test_db());
        st.open = true;
        st.run = Some(running(3, 12));
        let _ = st.update(Msg::Close);
        assert!(!st.open);
        assert!(st.is_running());
        assert_eq!(st.badge().as_deref(), Some("3/12"));
    }

    #[test]
    fn badge_has_no_count_before_the_total_is_known() {
        let mut st = State::new(test_db());
        st.run = Some(running(0, 0));
        assert_eq!(st.badge(), None);
        assert!(st.is_running());
    }

    #[test]
    fn finished_while_closed_is_reported_once() {
        let mut st = State::new(test_db());
        st.run = Some(running(12, 12));
        st.open = false;
        finish(&mut st, 7); // drives Msg::Run(RunEvent::Finished…) for session 7
        assert_eq!(st.take_finished().map(|(_, s, _)| s), Some(Some(7)));
        assert!(st.take_finished().is_none());
    }
}

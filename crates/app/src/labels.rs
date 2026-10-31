//! Labels screen: the label vocabulary with counts; rename, merge, delete
//! and bulk decisions on pending AI suggestions.

use iced::widget::{button, column, pick_list, row, rule, scrollable, text, text_input};
use iced::{Center, Element, Fill, Task as Cmd};
use juttusihti_core::{Label, LabelId, Status};

use crate::db::Db;
use crate::icons::Icon;
use crate::style::{self, SIZE_BODY, SIZE_SMALL, SPACE};

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Result<Vec<(Label, u64, u64)>, String>),
    NewName(String),
    Create,
    Select(LabelId),
    Rename(String),
    SaveRename,
    MergeTarget(Target),
    Merge,
    Delete,
    ConfirmDelete,
    Decide(Status),
    Done(Result<String, String>),
}

/// Pick-list entry for merge targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target(pub LabelId, pub String);

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}

pub struct State {
    db: Db,
    labels: Vec<(Label, u64, u64)>,
    new_name: String,
    selected: Option<LabelId>,
    rename: String,
    merge_target: Option<Target>,
    confirm_delete: bool,
    message: Option<Result<String, String>>,
}

impl State {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            labels: Vec::new(),
            new_name: String::new(),
            selected: None,
            rename: String::new(),
            merge_target: None,
            confirm_delete: false,
            message: None,
        }
    }

    pub fn load(&self) -> Cmd<Msg> {
        let db = self.db.clone();
        Cmd::perform(async move { db.call(|s| s.label_counts()).await }, Msg::Loaded)
    }

    fn run(
        &self,
        f: impl FnOnce(&mut juttusihti_core::store::Store) -> juttusihti_core::Result<String> + Send + 'static,
    ) -> Cmd<Msg> {
        let db = self.db.clone();
        Cmd::perform(async move { db.call(f).await }, Msg::Done)
    }

    pub fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Loaded(Ok(l)) => {
                self.labels = l;
                if self.selected.is_some_and(|id| !self.labels.iter().any(|(l, ..)| l.id == id)) {
                    self.selected = None;
                }
            }
            Msg::Loaded(Err(e)) => self.message = Some(Err(e)),
            Msg::NewName(s) => self.new_name = s,
            Msg::Create => {
                let name = self.new_name.trim().to_string();
                if name.is_empty() {
                    return Cmd::none();
                }
                self.new_name.clear();
                return self.run(move |s| {
                    let similar = s.similar_labels(&name)?;
                    s.create_label(&name)?;
                    Ok(match similar.first() {
                        Some(l) if l.name != name => format!("Created “{name}”. Similar existing label: “{}”.", l.name),
                        _ => format!("Created “{name}”."),
                    })
                });
            }
            Msg::Select(id) => {
                self.selected = Some(id);
                self.rename =
                    self.labels.iter().find(|(l, ..)| l.id == id).map(|(l, ..)| l.name.clone()).unwrap_or_default();
                self.merge_target = None;
                self.confirm_delete = false;
            }
            Msg::Rename(s) => self.rename = s,
            Msg::SaveRename => {
                let (Some(id), name) = (self.selected, self.rename.trim().to_string()) else { return Cmd::none() };
                return self.run(move |s| s.rename_label(id, &name).map(|_| format!("Renamed to “{name}”.")));
            }
            Msg::MergeTarget(t) => self.merge_target = Some(t),
            Msg::Merge => {
                let (Some(from), Some(Target(into, name))) = (self.selected, self.merge_target.clone()) else {
                    return Cmd::none();
                };
                self.selected = Some(into);
                return self.run(move |s| s.merge_labels(from, into).map(|_| format!("Merged into “{name}”.")));
            }
            Msg::Delete => self.confirm_delete = true,
            Msg::ConfirmDelete => {
                let Some(id) = self.selected.take() else { return Cmd::none() };
                self.confirm_delete = false;
                return self
                    .run(move |s| s.delete_label(id).map(|_| "Label deleted. Articles were not changed.".into()));
            }
            Msg::Decide(status) => {
                let Some(id) = self.selected else { return Cmd::none() };
                return self.run(move |s| {
                    let n = s.bulk_decide_label(id, status)?;
                    Ok(format!(
                        "{} {n} pending suggestions.",
                        if status == Status::Confirmed { "Accepted" } else { "Rejected" }
                    ))
                });
            }
            Msg::Done(r) => {
                self.message = Some(r);
                return self.load();
            }
        }
        Cmd::none()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let mut list = column![].spacing(2);
        for (l, confirmed, suggested) in &self.labels {
            let label = row![
                text(&l.name).size(SIZE_BODY).width(Fill),
                text(format!("{confirmed}")).size(SIZE_SMALL).style(style::muted).width(50),
                text(if *suggested > 0 { format!("+{suggested} AI") } else { String::new() })
                    .size(SIZE_SMALL)
                    .style(style::muted)
                    .width(70),
            ]
            .align_y(Center);
            list = list.push(
                button(label)
                    .width(Fill)
                    .padding([SPACE[1], SPACE[2]])
                    .style(if self.selected == Some(l.id) { style::row_selected } else { style::row })
                    .on_press(Msg::Select(l.id)),
            );
        }
        if self.labels.is_empty() {
            list = list.push(style::note("No labels yet. Create one below or label an article from its preview."));
        }
        let create = row![
            input("New label", &self.new_name, Msg::NewName).on_submit(Msg::Create),
            style::main_button("Create", Some(Msg::Create)),
        ]
        .spacing(SPACE[1])
        .align_y(Center);
        let left = column![
            column![style::title("Labels"), style::note("Counts: confirmed articles, then pending AI suggestions.")]
                .spacing(SPACE[1]),
            scrollable(list).height(Fill),
            create
        ]
        .spacing(SPACE[3])
        .padding(SPACE[4])
        .width(420);

        let right: Element<'_, Msg> = match self.labels.iter().find(|(l, ..)| Some(l.id) == self.selected) {
            None => style::note("Select a label to edit it.").into(),
            Some((l, confirmed, suggested)) => {
                let targets: Vec<Target> = self
                    .labels
                    .iter()
                    .filter(|(o, ..)| o.id != l.id)
                    .map(|(o, ..)| Target(o.id, o.name.clone()))
                    .collect();
                let mut col = column![
                    column![
                        style::title(&l.name),
                        style::note(format!("{confirmed} confirmed, {suggested} pending AI suggestions")),
                    ]
                    .spacing(SPACE[1]),
                    row![
                        input("Name", &self.rename, Msg::Rename).on_submit(Msg::SaveRename),
                        style::quiet_button(None, "Rename", style::quiet, Some(Msg::SaveRename)),
                    ]
                    .spacing(SPACE[1])
                    .align_y(Center),
                    row![
                        pick_list(targets, self.merge_target.clone(), Msg::MergeTarget)
                            .placeholder("Merge into…")
                            .text_size(SIZE_BODY),
                        style::quiet_button(
                            None,
                            "Merge",
                            style::quiet,
                            self.merge_target.as_ref().map(|_| Msg::Merge)
                        ),
                    ]
                    .spacing(SPACE[1])
                    .align_y(Center),
                    row![
                        style::quiet_button(
                            Some(Icon::Check),
                            "Accept all suggestions",
                            style::quiet,
                            (*suggested > 0).then_some(Msg::Decide(Status::Confirmed))
                        ),
                        style::quiet_button(
                            None,
                            "Reject all suggestions",
                            style::quiet_danger,
                            (*suggested > 0).then_some(Msg::Decide(Status::Rejected))
                        ),
                    ]
                    .spacing(SPACE[1]),
                ]
                .spacing(SPACE[4]);
                col = if self.confirm_delete {
                    col.push(
                        row![
                            text("Delete this label from all articles?").size(SIZE_SMALL),
                            style::quiet_button(None, "Delete", style::quiet_danger, Some(Msg::ConfirmDelete)),
                        ]
                        .spacing(SPACE[1])
                        .align_y(Center),
                    )
                } else {
                    col.push(style::quiet_button(None, "Delete label", style::quiet_danger, Some(Msg::Delete)))
                };
                col.into()
            }
        };
        let mut right_col = column![right].spacing(SPACE[3]).padding(SPACE[4]);
        if let Some(m) = &self.message {
            right_col = right_col.push(style::message(m));
        }
        row![left, rule::vertical(1), right_col].into()
    }
}

/// A text field in the screen's size.
fn input<'a>(placeholder: &'a str, value: &'a str, on: fn(String) -> Msg) -> iced::widget::TextInput<'a, Msg> {
    text_input(placeholder, value).on_input(on).size(SIZE_BODY).padding(SPACE[1])
}

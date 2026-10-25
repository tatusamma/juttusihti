//! Labels screen: the label vocabulary with counts; rename, merge, delete
//! and bulk decisions on pending AI suggestions.

use iced::widget::{button, column, container, pick_list, row, scrollable, text, text_input};
use iced::{Element, Fill, Font, Task as Cmd, font};
use juttusihti_core::{Label, LabelId, Status};

use crate::db::Db;

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
        let bold = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
        let mut list = column![].spacing(2);
        for (l, confirmed, suggested) in &self.labels {
            let label = row![
                text(&l.name).size(14).width(Fill),
                text(format!("{confirmed}")).size(12).width(50),
                text(if *suggested > 0 { format!("+{suggested} AI") } else { String::new() }).size(12).width(70),
            ];
            list = list.push(
                button(label)
                    .width(Fill)
                    .padding([6, 10])
                    .style(if self.selected == Some(l.id) { button::secondary } else { button::text })
                    .on_press(Msg::Select(l.id)),
            );
        }
        if self.labels.is_empty() {
            list = list.push(text("No labels yet. Create one below or label an article from its preview.").size(13));
        }
        let create = row![
            text_input("New label", &self.new_name).on_input(Msg::NewName).on_submit(Msg::Create),
            button(text("Create")).on_press(Msg::Create),
        ]
        .spacing(8);
        let left = column![
            text("Labels").size(24).font(bold),
            text("Counts: confirmed articles, then pending AI suggestions.").size(12),
            scrollable(list).height(Fill),
            create
        ]
        .spacing(10)
        .padding(24)
        .width(420);

        let right: Element<'_, Msg> = match self.labels.iter().find(|(l, ..)| Some(l.id) == self.selected) {
            None => text("Select a label to edit it.").size(14).into(),
            Some((l, confirmed, suggested)) => {
                let targets: Vec<Target> = self
                    .labels
                    .iter()
                    .filter(|(o, ..)| o.id != l.id)
                    .map(|(o, ..)| Target(o.id, o.name.clone()))
                    .collect();
                let mut col = column![
                    text(&l.name).size(20).font(bold),
                    text(format!("{confirmed} confirmed, {suggested} pending AI suggestions")).size(13),
                    row![
                        text_input("Name", &self.rename).on_input(Msg::Rename).on_submit(Msg::SaveRename),
                        button(text("Rename")).on_press(Msg::SaveRename)
                    ]
                    .spacing(8),
                    row![
                        pick_list(targets, self.merge_target.clone(), Msg::MergeTarget).placeholder("Merge into…"),
                        button(text("Merge"))
                            .style(button::secondary)
                            .on_press_maybe(self.merge_target.as_ref().map(|_| Msg::Merge)),
                    ]
                    .spacing(8),
                    row![
                        button(text("Accept all suggestions"))
                            .style(button::success)
                            .on_press_maybe((*suggested > 0).then_some(Msg::Decide(Status::Confirmed))),
                        button(text("Reject all suggestions"))
                            .style(button::secondary)
                            .on_press_maybe((*suggested > 0).then_some(Msg::Decide(Status::Rejected))),
                    ]
                    .spacing(8),
                ]
                .spacing(12);
                col = if self.confirm_delete {
                    col.push(
                        row![
                            text("Delete this label from all articles?").size(13),
                            button(text("Delete")).style(button::danger).on_press(Msg::ConfirmDelete)
                        ]
                        .spacing(8)
                        .align_y(iced::Center),
                    )
                } else {
                    col.push(button(text("Delete label")).style(button::danger).on_press(Msg::Delete))
                };
                col.into()
            }
        };
        let mut right_col = column![right].spacing(12).padding(24);
        match &self.message {
            Some(Ok(m)) => right_col = right_col.push(container(text(m).size(13)).padding(8).style(container::success)),
            Some(Err(m)) => right_col = right_col.push(container(text(m).size(13)).padding(8).style(container::danger)),
            None => {}
        }
        row![left, right_col].into()
    }
}

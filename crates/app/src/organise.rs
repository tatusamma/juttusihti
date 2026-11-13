//! Categories screen (organise): groups as headings, categories under them,
//! publisher terms behind "N terms ›", and the Unsorted and Ignored terms.
//! Rows move by dragging their ⋮⋮ grip or with their "Move to…" menu; both
//! go through [`State::drop_op`], so they always do the same thing.

use std::collections::{BTreeSet, HashMap};

use iced::keyboard::{self, key};
use iced::widget::{Space, button, column, container, operation, pick_list, row, scrollable, text, text_input};
use iced::{Center, Element, Fill, Point, Subscription, Task as Cmd, event};
use juttusihti_core::categories::{CategoryNode, GroupId, GroupNode, TermId, TermRow};
use juttusihti_core::category_draft::CategoryDraft;
use juttusihti_core::pricing::Catalog;
use juttusihti_core::store::Store;
use juttusihti_core::{LabelId, Status};

use crate::db::Db;
use crate::drag::{self, DragMsg, DragState};
use crate::icons::{Icon, icon};
use crate::organise_draft::{self as ai, Draft, DraftMsg, Outcome, Panel, SuggestPlan};
use crate::style::{self, SIZE_BODY, SIZE_SMALL, SPACE};

/// Something that can be dragged or moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgItem {
    Group(GroupId),
    Category(LabelId),
    Term(TermId),
}

/// Where a dragged row can land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgTarget {
    /// In a group (`None`: Ungrouped) before its row at this index. For a
    /// dragged group, `Some(g)` means before group `g` and `None` the end.
    GroupSlot(Option<GroupId>, usize),
    Category(LabelId),
    Unsorted,
    Ignored,
}

/// A "Move to…" entry: where the row goes and how the menu names it.
#[derive(Debug, Clone, PartialEq)]
pub struct Dest(pub OrgTarget, pub String);

impl std::fmt::Display for Dest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}

/// Pick-list entry for merge targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target(pub LabelId, pub String);

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}

/// A deletion waiting for confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirm {
    Group(GroupId),
    Category(LabelId),
}

/// The name field open inline, with what is typed so far.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Editing {
    NewGroup(String),
    NewCategory(String),
    Group(GroupId, String),
    Category(LabelId, String),
}

/// Everything the screen shows, loaded in one call.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub(crate) groups: Vec<GroupNode>,
    pub(crate) ungrouped: Vec<CategoryNode>,
    /// Confirmed articles and pending AI label suggestions per category.
    counts: HashMap<LabelId, (u64, u64)>,
    /// Terms of the opened categories.
    terms: HashMap<LabelId, Vec<TermRow>>,
    pub(crate) unsorted: Vec<TermRow>,
    ignored: Vec<TermRow>,
    /// The AI draft waiting for Approve or Cancel.
    pending: Option<Box<(i64, CategoryDraft)>>,
}

fn snapshot(s: &Store, expanded: &BTreeSet<LabelId>) -> juttusihti_core::Result<Snapshot> {
    let tree = s.category_tree()?;
    let counts =
        s.label_counts()?.into_iter().map(|(l, confirmed, suggested)| (l.id, (confirmed, suggested))).collect();
    let mut terms = HashMap::new();
    for &id in expanded {
        terms.insert(id, s.category_terms(id)?);
    }
    Ok(Snapshot {
        groups: tree.groups,
        ungrouped: tree.ungrouped,
        counts,
        terms,
        unsorted: s.unsorted_terms()?,
        ignored: s.ignored_terms()?,
        pending: s.pending_category_draft()?.map(Box::new),
    })
}

/// Store work asked for by the screen.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    CreateGroup(String),
    CreateCategory(String),
    RenameGroup(GroupId, String),
    DeleteGroup(GroupId, String),
    MoveGroup(GroupId, usize),
    MoveCategory(LabelId, Option<GroupId>, usize),
    RenameCategory(LabelId, String),
    Merge(LabelId, LabelId, String),
    DeleteCategory(LabelId),
    Decide(LabelId, Status),
    MapTerm(TermId, Option<LabelId>),
    Ignore(TermId, bool),
    SaveDraft(i64, Box<CategoryDraft>),
    ApproveDraft(i64, Box<CategoryDraft>),
    DiscardDraft(i64),
}

impl Op {
    /// Runs the work; the text is shown on the screen (empty: nothing to say).
    fn apply(self, s: &mut Store) -> juttusihti_core::Result<String> {
        match self {
            Op::CreateGroup(name) => s.create_group(&name).map(|_| String::new()),
            Op::CreateCategory(name) => {
                let similar = s.similar_labels(&name)?;
                s.create_label(&name)?;
                Ok(match similar.first() {
                    Some(l) if l.name != name => format!("Created “{name}”. Similar existing category: “{}”.", l.name),
                    _ => format!("Created “{name}”."),
                })
            }
            Op::RenameGroup(id, name) => s.rename_group(id, &name).map(|_| format!("Renamed to “{name}”.")),
            Op::DeleteGroup(id, name) => {
                s.delete_group(id).map(|_| format!("Deleted group “{name}”. Its categories are now Ungrouped."))
            }
            Op::MoveGroup(id, index) => s.move_group(id, index).map(|_| String::new()),
            Op::MoveCategory(id, group, index) => s.move_category(id, group, index).map(|_| String::new()),
            Op::RenameCategory(id, name) => s.rename_label(id, &name).map(|_| format!("Renamed to “{name}”.")),
            Op::Merge(from, into, name) => s.merge_labels(from, into).map(|_| format!("Merged into “{name}”.")),
            Op::DeleteCategory(id) => s.delete_label(id).map(|_| "Category deleted. Articles were not changed.".into()),
            Op::Decide(id, status) => {
                let n = s.bulk_decide_label(id, status)?;
                Ok(format!(
                    "{} {n} pending suggestions.",
                    if status == Status::Confirmed { "Accepted" } else { "Rejected" }
                ))
            }
            Op::MapTerm(id, label) => s.map_term(id, label).map(|_| String::new()),
            Op::Ignore(id, ignored) => s.set_term_ignored(id, ignored).map(|_| String::new()),
            Op::SaveDraft(id, draft) => s.update_category_draft(id, &draft).map(|_| String::new()),
            Op::ApproveDraft(id, draft) => s.apply_category_draft(id, &draft).map(ai::applied_text),
            Op::DiscardDraft(id) => s.discard_category_draft(id).map(|_| String::new()),
        }
    }
}

/// What a message asks the runtime to do next.
#[derive(Debug, PartialEq)]
enum Effect {
    None,
    Load,
    Run(Op),
    /// Focus the inline name field.
    Focus,
    /// Work out the AI estimate for the consent panel (this generation).
    Plan(u64),
    /// Send the consented AI request.
    Suggest,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Result<Snapshot, String>),
    Done(Result<String, String>),
    Drag(DragMsg<OrgItem, OrgTarget>),
    MoveTo(OrgItem, Dest),
    ToggleTerms(LabelId),
    ToggleIgnored,
    NewGroup,
    NewCategory,
    /// Click on a group or category name.
    StartRename(OrgItem),
    EditName(String),
    SaveName,
    Escape,
    AskDelete(Confirm),
    ConfirmDelete,
    CancelDelete,
    /// Opens or closes a category's merge, bulk-decide and delete actions.
    Actions(LabelId),
    MergeTarget(Target),
    Merge,
    Decide(Status),
    Ignore(TermId, bool),
    /// "Suggest categories": opens the consent panel.
    Suggest,
    Planned(u64, Result<Box<SuggestPlan>, String>),
    /// The consent: sends the request.
    SendSuggest,
    CloseSuggest,
    /// The saved draft's id, the draft and the terms left for later.
    Suggested(Result<(i64, Box<CategoryDraft>, usize), String>),
    Draft(DraftMsg),
}

pub(crate) const NAME_ID: &str = "organise-name";

/// Left indent of category rows under their group heading, and of terms under their category.
const INDENT: f32 = 22.0;

pub struct State {
    db: Db,
    snap: Snapshot,
    loaded: bool,
    expanded: BTreeSet<LabelId>,
    show_ignored: bool,
    drag: DragState<OrgItem, OrgTarget>,
    editing: Option<Editing>,
    actions: Option<LabelId>,
    merge_target: Option<Target>,
    confirm: Option<Confirm>,
    message: Option<Result<String, String>>,
    catalog: Catalog,
    /// The AI consent panel, while open.
    suggest: Option<Panel>,
    /// Drops estimates of a panel that was closed meanwhile.
    generation: u64,
    /// The AI draft; while open it replaces the normal organising controls.
    draft: Option<Draft>,
    /// Terms left for a later run, for the draft of this id.
    more_terms: Option<(i64, usize)>,
}

impl State {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            snap: Snapshot::default(),
            loaded: false,
            expanded: BTreeSet::new(),
            show_ignored: false,
            drag: DragState::default(),
            editing: None,
            actions: None,
            merge_target: None,
            confirm: None,
            message: None,
            catalog: Catalog::builtin(),
            suggest: None,
            generation: 0,
            draft: None,
            more_terms: None,
        }
    }

    pub fn load(&self) -> Cmd<Msg> {
        let db = self.db.clone();
        let expanded = self.expanded.clone();
        Cmd::perform(async move { db.call(move |s| snapshot(s, &expanded)).await }, Msg::Loaded)
    }

    pub fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match self.handle(msg) {
            Effect::None => Cmd::none(),
            Effect::Load => self.load(),
            Effect::Run(op) => {
                let db = self.db.clone();
                Cmd::perform(async move { db.call(move |s| op.apply(s)).await }, Msg::Done)
            }
            Effect::Focus => operation::focus(NAME_ID).chain(operation::select_all(NAME_ID)),
            Effect::Plan(generation) => {
                Cmd::perform(ai::plan(self.db.clone(), self.catalog.clone()), move |r| Msg::Planned(generation, r))
            }
            Effect::Suggest => match &self.suggest {
                Some(Panel::Running(p)) => Cmd::perform(ai::run(self.db.clone(), (**p).clone()), Msg::Suggested),
                _ => Cmd::none(),
            },
        }
    }

    /// Pointer tracking while a drag is armed, and Esc (even from inside the
    /// name field, which keeps Esc from the app's key handler) while anything
    /// is open.
    pub fn subscription(&self) -> Subscription<Msg> {
        let dragging = self.drag.dragging().is_some();
        let draft_dragging = self.draft.as_ref().is_some_and(Draft::dragging);
        let pointer = if draft_dragging {
            drag::subscription(
                |p| Msg::Draft(DraftMsg::Drag(DragMsg::Move(p))),
                || Msg::Draft(DraftMsg::Drag(DragMsg::Release)),
            )
        } else if dragging {
            drag::subscription(|p| Msg::Drag(DragMsg::Move(p)), || Msg::Drag(DragMsg::Release))
        } else {
            Subscription::none()
        };
        let draft_busy = self.draft.as_ref().is_some_and(Draft::busy);
        let escape = if draft_busy || dragging || self.editing.is_some() || self.confirm.is_some() {
            event::listen_with(|e, _status, _window| match e {
                iced::Event::Keyboard(keyboard::Event::KeyPressed {
                    key: keyboard::Key::Named(key::Named::Escape),
                    ..
                }) => Some(Msg::Escape),
                _ => None,
            })
        } else {
            Subscription::none()
        };
        Subscription::batch([pointer, escape])
    }

    /// The label and cursor position of the chip to draw at the pointer, while dragging.
    pub fn ghost(&self) -> Option<(&str, Point)> {
        if let Some(d) = &self.draft {
            return d.ghost();
        }
        self.drag.dragging().filter(|d| d.active).map(|d| (d.label.as_str(), d.cursor))
    }

    fn handle(&mut self, msg: Msg) -> Effect {
        match msg {
            Msg::Loaded(Ok(mut snap)) => {
                self.sync_draft(snap.pending.take().map(|p| *p));
                self.snap = snap;
                self.loaded = true;
                let gone: Vec<LabelId> =
                    self.expanded.iter().copied().filter(|id| self.category(*id).is_none()).collect();
                for id in gone {
                    self.expanded.remove(&id);
                }
                if self.actions.is_some_and(|id| self.category(id).is_none()) {
                    self.actions = None;
                }
            }
            Msg::Loaded(Err(e)) => self.message = Some(Err(e)),
            Msg::Done(r) => {
                self.message = match r {
                    Ok(m) if m.is_empty() => None,
                    r => Some(r),
                };
                return Effect::Load;
            }
            Msg::Drag(m) => {
                if let Some((item, target)) = self.drag.update(m) {
                    return self.drop_op(item, target).map_or(Effect::None, Effect::Run);
                }
            }
            Msg::MoveTo(item, Dest(target, _)) => {
                return self.drop_op(item, target).map_or(Effect::None, Effect::Run);
            }
            Msg::ToggleTerms(id) => {
                if !self.expanded.remove(&id) {
                    self.expanded.insert(id);
                    return Effect::Load;
                }
            }
            Msg::ToggleIgnored => self.show_ignored = !self.show_ignored,
            Msg::NewGroup => {
                self.editing = Some(Editing::NewGroup(String::new()));
                return Effect::Focus;
            }
            Msg::NewCategory => {
                self.editing = Some(Editing::NewCategory(String::new()));
                return Effect::Focus;
            }
            Msg::StartRename(item) => {
                self.editing = match item {
                    OrgItem::Group(id) => self.group(id).map(|g| Editing::Group(id, g.group.name.clone())),
                    OrgItem::Category(id) => self.category(id).map(|c| Editing::Category(id, c.label.name.clone())),
                    OrgItem::Term(_) => None,
                };
                return if self.editing.is_some() { Effect::Focus } else { Effect::None };
            }
            Msg::EditName(s) => {
                if let Some(
                    Editing::NewGroup(n) | Editing::NewCategory(n) | Editing::Group(_, n) | Editing::Category(_, n),
                ) = &mut self.editing
                {
                    *n = s;
                }
            }
            Msg::SaveName => {
                let Some(editing) = self.editing.take() else { return Effect::None };
                let op = match editing {
                    Editing::NewGroup(n) if !n.trim().is_empty() => Op::CreateGroup(n.trim().into()),
                    Editing::NewCategory(n) if !n.trim().is_empty() => Op::CreateCategory(n.trim().into()),
                    Editing::Group(id, n) if self.group(id).is_some_and(|g| renamed(&g.group.name, &n)) => {
                        Op::RenameGroup(id, n.trim().into())
                    }
                    Editing::Category(id, n) if self.category(id).is_some_and(|c| renamed(&c.label.name, &n)) => {
                        Op::RenameCategory(id, n.trim().into())
                    }
                    _ => return Effect::None,
                };
                return Effect::Run(op);
            }
            Msg::Escape => {
                if let Some(d) = self.draft.as_mut() {
                    d.update(DraftMsg::Escape, &self.snap);
                }
                self.drag.update(DragMsg::Cancel);
                self.editing = None;
                self.confirm = None;
            }
            Msg::AskDelete(c) => self.confirm = Some(c),
            Msg::ConfirmDelete => {
                let op = match self.confirm.take() {
                    Some(Confirm::Group(id)) => {
                        Op::DeleteGroup(id, self.group(id).map(|g| g.group.name.clone()).unwrap_or_default())
                    }
                    Some(Confirm::Category(id)) => {
                        self.actions = None;
                        Op::DeleteCategory(id)
                    }
                    None => return Effect::None,
                };
                return Effect::Run(op);
            }
            Msg::CancelDelete => self.confirm = None,
            Msg::Actions(id) => {
                self.actions = if self.actions == Some(id) { None } else { Some(id) };
                self.merge_target = None;
                self.confirm = None;
            }
            Msg::MergeTarget(t) => self.merge_target = Some(t),
            Msg::Merge => {
                let (Some(from), Some(Target(into, name))) = (self.actions, self.merge_target.take()) else {
                    return Effect::None;
                };
                self.actions = None;
                return Effect::Run(Op::Merge(from, into, name));
            }
            Msg::Decide(status) => {
                if let Some(id) = self.actions {
                    return Effect::Run(Op::Decide(id, status));
                }
            }
            Msg::Ignore(id, ignored) => return Effect::Run(Op::Ignore(id, ignored)),
            Msg::Suggest
            | Msg::Planned(..)
            | Msg::SendSuggest
            | Msg::CloseSuggest
            | Msg::Suggested(_)
            | Msg::Draft(_) => return self.handle_ai(msg),
        }
        Effect::None
    }

    /// The AI suggestion messages: consent panel, run and draft.
    fn handle_ai(&mut self, msg: Msg) -> Effect {
        match msg {
            Msg::Suggest => {
                if self.draft.is_some() || matches!(self.suggest, Some(Panel::Planning | Panel::Running(_))) {
                    return Effect::None;
                }
                self.generation += 1;
                self.suggest = Some(Panel::Planning);
                return Effect::Plan(self.generation);
            }
            Msg::Planned(g, r) => {
                if g == self.generation && matches!(self.suggest, Some(Panel::Planning | Panel::Ready(_))) {
                    self.suggest = Some(match r {
                        Ok(p) => Panel::Ready(p),
                        Err(e) => Panel::Failed(e),
                    });
                }
            }
            Msg::SendSuggest => {
                if let Some(Panel::Ready(p)) = &self.suggest
                    && p.blocker().is_none()
                {
                    self.suggest = Some(Panel::Running(p.clone()));
                    return Effect::Suggest;
                }
            }
            Msg::CloseSuggest => {
                if !matches!(self.suggest, Some(Panel::Running(_))) {
                    self.suggest = None;
                }
            }
            Msg::Suggested(Ok((id, draft, more))) => {
                self.suggest = None;
                self.more_terms = Some((id, more));
                self.draft = Some(Draft::new(id, *draft, more));
                return Effect::Load;
            }
            Msg::Suggested(Err(e)) => self.suggest = Some(Panel::Failed(e)),
            Msg::Draft(m) => {
                let Some(d) = self.draft.as_mut() else { return Effect::None };
                return match d.update(m, &self.snap) {
                    Outcome::None => Effect::None,
                    Outcome::Focus => Effect::Focus,
                    Outcome::Changed => Effect::Run(Op::SaveDraft(d.id, Box::new(d.plan.clone()))),
                    Outcome::Approve => Effect::Run(Op::ApproveDraft(d.id, Box::new(d.plan.clone()))),
                    Outcome::Discard => Effect::Run(Op::DiscardDraft(d.id)),
                };
            }
            _ => {}
        }
        Effect::None
    }

    /// Opens the stored draft, or closes the shown one once it is approved or
    /// discarded. The in-memory draft wins over the stored one, which may lag
    /// behind an edit still being saved.
    fn sync_draft(&mut self, pending: Option<(i64, CategoryDraft)>) {
        match (&self.draft, pending) {
            (Some(d), Some((id, _))) if d.id == id => {}
            (_, Some((id, plan))) => {
                let more = self.more_terms.filter(|(m, _)| *m == id).map_or(0, |(_, n)| n);
                self.draft = Some(Draft::new(id, plan, more));
                self.suggest = None;
            }
            (_, None) => self.draft = None,
        }
    }

    fn group(&self, id: GroupId) -> Option<&GroupNode> {
        self.snap.groups.iter().find(|g| g.group.id == id)
    }

    fn categories(&self) -> impl Iterator<Item = &CategoryNode> {
        self.snap.groups.iter().flat_map(|g| &g.categories).chain(&self.snap.ungrouped)
    }

    fn category(&self, id: LabelId) -> Option<&CategoryNode> {
        self.categories().find(|c| c.label.id == id)
    }

    /// The categories of a group, or the ungrouped ones.
    fn members(&self, group: Option<GroupId>) -> &[CategoryNode] {
        match group {
            Some(g) => self.group(g).map_or(&[], |g| &g.categories),
            None => &self.snap.ungrouped,
        }
    }

    /// The store work for dropping `item` on `target`; `None` when nothing would change.
    fn drop_op(&self, item: OrgItem, target: OrgTarget) -> Option<Op> {
        match (item, target) {
            (OrgItem::Group(id), OrgTarget::GroupSlot(before, _)) => {
                let index = |g: GroupId| self.snap.groups.iter().position(|x| x.group.id == g);
                let from = index(id)?;
                let to = match before {
                    Some(b) => {
                        let to = index(b)?;
                        if from < to { to - 1 } else { to }
                    }
                    None => self.snap.groups.len() - 1,
                };
                (to != from).then_some(Op::MoveGroup(id, to))
            }
            (OrgItem::Category(id), OrgTarget::GroupSlot(group, index)) => {
                self.category(id)?;
                match self.members(group).iter().position(|c| c.label.id == id) {
                    // Within its own group the row leaves its old place first.
                    Some(from) => {
                        let to = if from < index { index - 1 } else { index };
                        (to != from).then_some(Op::MoveCategory(id, group, to))
                    }
                    None => Some(Op::MoveCategory(id, group, index)),
                }
            }
            (OrgItem::Term(id), OrgTarget::Category(label)) => Some(Op::MapTerm(id, Some(label))),
            (OrgItem::Term(id), OrgTarget::Unsorted) => Some(Op::MapTerm(id, None)),
            (OrgItem::Term(id), OrgTarget::Ignored) => Some(Op::Ignore(id, true)),
            _ => None,
        }
    }

    /// Where a row's "Move to…" menu can send it, leaving out where it already is.
    fn dests(&self, item: OrgItem) -> Vec<Dest> {
        match item {
            OrgItem::Group(id) => {
                let mut out: Vec<Dest> = self
                    .snap
                    .groups
                    .iter()
                    .filter(|g| g.group.id != id)
                    .map(|g| Dest(OrgTarget::GroupSlot(Some(g.group.id), 0), format!("Before “{}”", g.group.name)))
                    .collect();
                out.push(Dest(OrgTarget::GroupSlot(None, 0), "To the end".into()));
                out.retain(|d| self.drop_op(item, d.0).is_some());
                out
            }
            OrgItem::Category(id) => {
                let current = self.category(id).and_then(|c| c.group_id);
                let mut out: Vec<Dest> = self
                    .snap
                    .groups
                    .iter()
                    .filter(|g| Some(g.group.id) != current)
                    .map(|g| Dest(OrgTarget::GroupSlot(Some(g.group.id), g.categories.len()), g.group.name.clone()))
                    .collect();
                if current.is_some() {
                    out.push(Dest(OrgTarget::GroupSlot(None, self.snap.ungrouped.len()), "Ungrouped".into()));
                }
                // Reordering within its own group, as dropping on a sibling row does.
                let siblings = self.members(current);
                let place = current.and_then(|g| self.group(g)).map_or("Ungrouped", |g| g.group.name.as_str());
                let reorder = siblings
                    .iter()
                    .enumerate()
                    .map(|(i, c)| Dest(OrgTarget::GroupSlot(current, i), format!("Before “{}”", c.label.name)))
                    .chain([Dest(OrgTarget::GroupSlot(current, siblings.len()), format!("To the end of “{place}”"))])
                    .filter(|d| self.drop_op(item, d.0).is_some());
                out.extend(reorder);
                out
            }
            OrgItem::Term(id) => {
                let unsorted = self.snap.unsorted.iter().any(|t| t.id == id);
                let ignored = self.snap.ignored.iter().any(|t| t.id == id);
                let current = self.snap.terms.iter().find(|(_, ts)| ts.iter().any(|t| t.id == id)).map(|(l, _)| *l);
                let mut out: Vec<Dest> = self
                    .categories()
                    .filter(|c| Some(c.label.id) != current)
                    .map(|c| Dest(OrgTarget::Category(c.label.id), c.label.name.clone()))
                    .collect();
                if !unsorted {
                    out.push(Dest(OrgTarget::Unsorted, "Unsorted".into()));
                }
                if !ignored {
                    out.push(Dest(OrgTarget::Ignored, "Ignored".into()));
                }
                out
            }
        }
    }

    /// The question shown while a deletion waits for confirmation.
    fn confirm_text(&self) -> Option<String> {
        Some(match self.confirm? {
            Confirm::Group(id) => {
                format!("Delete group “{}”? Its categories become Ungrouped.", self.group(id)?.group.name)
            }
            Confirm::Category(_) => "Delete this category from all articles?".into(),
        })
    }

    // ---- view ----------------------------------------------------------

    pub fn view(&self) -> Element<'_, Msg> {
        if let Some(d) = &self.draft {
            return d.view(&self.snap, self.message.as_ref()).map(Msg::Draft);
        }
        // While a drag is armed, rows become the drop zones that fit what is dragged.
        let kind = self.drag.dragging().map(|d| d.item);
        let mut body = column![].spacing(SPACE[4]);
        if self.loaded && self.categories().next().is_none() {
            body =
                body.push(style::note("No categories yet. Publisher categories appear here after the next refresh."));
        }
        for g in &self.snap.groups {
            body = body.push(self.view_group(g, kind));
        }
        body = body.push(self.view_new_group());
        if !self.snap.groups.is_empty() || !self.snap.ungrouped.is_empty() {
            body = body.push(self.view_ungrouped(kind));
        }
        body = body.push(self.view_unsorted(kind));
        if !self.snap.ignored.is_empty() || matches!(kind, Some(OrgItem::Term(_))) {
            body = body.push(self.view_ignored(kind));
        }

        let header = row![
            style::title("Categories"),
            Space::new().width(Fill),
            style::quiet_button(
                Some(Icon::Sparkles),
                "Suggest categories",
                style::quiet,
                (self.suggest.is_none() && self.loaded).then_some(Msg::Suggest),
            ),
        ]
        .align_y(Center);
        let mut page = column![header].spacing(SPACE[3]).max_width(720.0);
        if let Some(panel) = &self.suggest {
            page = page.push(ai::view_panel(panel));
        }
        if let Some(m) = &self.message {
            page = page.push(style::message(m));
        }
        page = page.push(scrollable(container(body).padding(iced::padding::right(SPACE[3]))).height(Fill));
        container(page).padding(SPACE[4]).width(Fill).height(Fill).into()
    }

    fn over(&self, t: OrgTarget) -> bool {
        self.drag.dragging().is_some_and(|d| d.active && d.over == Some(t))
    }

    /// A drop zone with an outline.
    fn zone<'a>(&self, content: impl Into<Element<'a, Msg>>, t: OrgTarget) -> Element<'a, Msg> {
        drag::target(content.into(), self.over(t), Msg::Drag(DragMsg::Enter(t)), Msg::Drag(DragMsg::Leave(t)))
    }

    /// A drop zone shown as a line above it.
    fn slot<'a>(&self, content: impl Into<Element<'a, Msg>>, t: OrgTarget) -> Element<'a, Msg> {
        drag::insertion(content.into(), self.over(t), Msg::Drag(DragMsg::Enter(t)), Msg::Drag(DragMsg::Leave(t)))
    }

    fn grip<'a>(&self, item: OrgItem, label: &str, content: impl Into<Element<'a, Msg>>) -> Element<'a, Msg> {
        let label = label.to_string();
        drag::handle(content.into(), move |p| Msg::Drag(DragMsg::Press(item, label.clone(), p)))
    }

    /// A group or category name: click to rename, or the open name field.
    fn name<'a>(&'a self, item: OrgItem, name: &'a str, strong: bool) -> Element<'a, Msg> {
        let typed = match (&self.editing, item) {
            (Some(Editing::Group(id, n)), OrgItem::Group(g)) if *id == g => Some(n),
            (Some(Editing::Category(id, n)), OrgItem::Category(c)) if *id == c => Some(n),
            _ => None,
        };
        if let Some(n) = typed {
            return name_input("Name", n).into();
        }
        let label = text(name).size(SIZE_BODY);
        let label = if strong { label.font(style::SEMIBOLD) } else { label };
        button(label).padding([0.0, SPACE[0]]).style(style::row).on_press(Msg::StartRename(item)).into()
    }

    /// The row's "Move to…" menu (the keyboard way to do what dragging does).
    fn move_to(&self, item: OrgItem) -> Element<'_, Msg> {
        let dests = self.dests(item);
        if dests.is_empty() {
            return Space::new().into();
        }
        pick_list(dests, None::<Dest>, move |d| Msg::MoveTo(item, d))
            .placeholder("Move to…")
            .text_size(SIZE_SMALL)
            .padding([2.0, SPACE[1]])
            .style(style::pick_list)
            .menu_style(style::menu)
            .into()
    }

    fn confirm_row(&self) -> Element<'_, Msg> {
        row![
            text(self.confirm_text().unwrap_or_default()).size(SIZE_SMALL),
            style::quiet_button(None, "Delete", style::quiet_danger, Some(Msg::ConfirmDelete)),
            style::quiet_button(None, "Cancel", style::quiet, Some(Msg::CancelDelete)),
        ]
        .spacing(SPACE[1])
        .align_y(Center)
        .into()
    }

    fn view_group<'a>(&'a self, g: &'a GroupNode, kind: Option<OrgItem>) -> Element<'a, Msg> {
        let id = g.group.id;
        let item = OrgItem::Group(id);
        let heading = row![
            self.grip(item, &g.group.name, self.name(item, &g.group.name, true)),
            Space::new().width(Fill),
            self.move_to(item),
            style::icon_button(Icon::Close, "Delete group", SIZE_SMALL, Some(Msg::AskDelete(Confirm::Group(id)))),
        ]
        .spacing(SPACE[1])
        .align_y(Center);
        let mut block = column![heading].spacing(SPACE[0]);
        if self.confirm == Some(Confirm::Group(id)) {
            block = block.push(self.confirm_row());
        }
        block = block.push(self.view_members(Some(id), &g.categories, kind));
        match kind {
            Some(OrgItem::Group(_)) => self.slot(block, OrgTarget::GroupSlot(Some(id), 0)),
            Some(OrgItem::Category(_)) => self.zone(block, OrgTarget::GroupSlot(Some(id), g.categories.len())),
            _ => block.into(),
        }
    }

    /// "+ New group" and "+ New category", each opening its inline name field.
    fn view_new_group(&self) -> Element<'_, Msg> {
        let add = |label: &'static str, msg: Msg| -> Element<'_, Msg> {
            button(text(label).size(SIZE_SMALL).style(style::accent))
                .padding([0.0, SPACE[0]])
                .style(style::row)
                .on_press(msg)
                .into()
        };
        let group = match &self.editing {
            Some(Editing::NewGroup(n)) => name_input("Group name", n).into(),
            _ => add("+ New group", Msg::NewGroup),
        };
        // A new category starts Ungrouped.
        let category = match &self.editing {
            Some(Editing::NewCategory(n)) => name_input("Category name", n).into(),
            _ => add("+ New category", Msg::NewCategory),
        };
        row![group, category].spacing(SPACE[3]).align_y(Center).into()
    }

    fn view_ungrouped(&self, kind: Option<OrgItem>) -> Element<'_, Msg> {
        let cats = &self.snap.ungrouped;
        let block = column![style::heading("Ungrouped"), self.view_members(None, cats, kind)].spacing(SPACE[0]);
        match kind {
            // Above Ungrouped is after the last group.
            Some(OrgItem::Group(_)) => self.slot(block, OrgTarget::GroupSlot(None, 0)),
            Some(OrgItem::Category(_)) => self.zone(block, OrgTarget::GroupSlot(None, cats.len())),
            _ => block.into(),
        }
    }

    fn view_members<'a>(
        &'a self,
        group: Option<GroupId>,
        cats: &'a [CategoryNode],
        kind: Option<OrgItem>,
    ) -> Element<'a, Msg> {
        let mut col = column![];
        for (i, c) in cats.iter().enumerate() {
            let r = self.view_category(c);
            col = col.push(match kind {
                Some(OrgItem::Category(_)) => self.slot(r, OrgTarget::GroupSlot(group, i)),
                Some(OrgItem::Term(_)) => self.zone(r, OrgTarget::Category(c.label.id)),
                _ => r,
            });
        }
        if cats.is_empty() {
            let empty = if group.is_some() { "Empty. Drag categories here." } else { "Nothing ungrouped." };
            col = col.push(
                container(text(empty).size(SIZE_SMALL).style(style::faint))
                    .padding(iced::padding::left(INDENT).top(SPACE[0]).bottom(SPACE[0])),
            );
        }
        col.into()
    }

    fn view_category<'a>(&'a self, c: &'a CategoryNode) -> Element<'a, Msg> {
        let id = c.label.id;
        let item = OrgItem::Category(id);
        let open = self.expanded.contains(&id);
        let (_, pending) = self.snap.counts.get(&id).copied().unwrap_or_default();
        let terms: Element<'_, Msg> = match c.term_count {
            0 => text("No terms").size(SIZE_SMALL).style(style::faint).into(),
            n => button(
                row![
                    text(format!("{n} {}", if n == 1 { "term" } else { "terms" })).size(SIZE_SMALL).style(style::muted),
                    icon(if open { Icon::ChevronDown } else { Icon::ChevronRight })
                        .size(SIZE_SMALL)
                        .style(style::muted),
                ]
                .spacing(SPACE[0])
                .align_y(Center),
            )
            .padding([0.0, SPACE[0]])
            .style(style::row)
            .on_press(Msg::ToggleTerms(id))
            .into(),
        };
        let mut line = row![self.grip(item, &c.label.name, self.name(item, &c.label.name, false))]
            .spacing(SPACE[1])
            .align_y(Center);
        if pending > 0 {
            line = line.push(text(format!("+{pending} AI")).size(SIZE_SMALL).style(style::accent));
        }
        line = line.push(Space::new().width(Fill)).push(terms).push(self.move_to(item)).push(style::icon_button(
            Icon::More,
            "Merge, delete…",
            SIZE_SMALL,
            Some(Msg::Actions(id)),
        ));
        let mut col = column![container(line).padding(iced::padding::left(INDENT).top(2.0).bottom(2.0))];
        if self.actions == Some(id) {
            col = col.push(self.view_actions(c));
        }
        if open {
            for t in self.snap.terms.get(&id).into_iter().flatten() {
                col = col.push(self.view_term(t, false, 2.0 * INDENT));
            }
        }
        col.into()
    }

    /// Today's label actions for one category: merge, bulk decisions, delete.
    fn view_actions<'a>(&'a self, c: &'a CategoryNode) -> Element<'a, Msg> {
        let id = c.label.id;
        let (confirmed, pending) = self.snap.counts.get(&id).copied().unwrap_or_default();
        let targets: Vec<Target> =
            self.categories().filter(|o| o.label.id != id).map(|o| Target(o.label.id, o.label.name.clone())).collect();
        let merge = row![
            pick_list(targets, self.merge_target.clone(), Msg::MergeTarget)
                .style(style::pick_list)
                .menu_style(style::menu)
                .placeholder("Merge into…")
                .text_size(SIZE_SMALL),
            style::quiet_button(None, "Merge", style::quiet, self.merge_target.as_ref().map(|_| Msg::Merge)),
        ]
        .spacing(SPACE[1])
        .align_y(Center);
        let decide = row![
            style::quiet_button(
                Some(Icon::Check),
                "Accept all suggestions",
                style::quiet,
                (pending > 0).then_some(Msg::Decide(Status::Confirmed))
            ),
            style::quiet_button(
                None,
                "Reject all suggestions",
                style::quiet_danger,
                (pending > 0).then_some(Msg::Decide(Status::Rejected))
            ),
        ]
        .spacing(SPACE[1]);
        let delete = if self.confirm == Some(Confirm::Category(id)) {
            self.confirm_row()
        } else {
            style::quiet_button(
                None,
                "Delete category",
                style::quiet_danger,
                Some(Msg::AskDelete(Confirm::Category(id))),
            )
        };
        container(
            column![
                style::note(format!("{confirmed} confirmed, {pending} pending AI suggestions")),
                merge,
                decide,
                delete
            ]
            .spacing(SPACE[1]),
        )
        .padding(iced::padding::left(2.0 * INDENT).top(SPACE[1]).bottom(SPACE[2]))
        .into()
    }

    /// A publisher term with its feed in grey; `ignored` offers Un-ignore instead of Ignore.
    fn view_term<'a>(&'a self, t: &'a TermRow, ignored: bool, indent: f32) -> Element<'a, Msg> {
        let item = OrgItem::Term(t.id);
        let name = row![text(&t.name).size(SIZE_BODY), text(&t.feed_title).size(SIZE_SMALL).style(style::muted)]
            .spacing(SPACE[1])
            .align_y(Center);
        let action = if ignored {
            style::quiet_button(None, "Un-ignore", style::quiet, Some(Msg::Ignore(t.id, false)))
        } else {
            style::quiet_button(None, "Ignore", style::quiet, Some(Msg::Ignore(t.id, true)))
        };
        container(
            row![self.grip(item, &t.name, name), Space::new().width(Fill), action, self.move_to(item)]
                .spacing(SPACE[1])
                .align_y(Center),
        )
        .padding(iced::padding::left(indent).top(2.0).bottom(2.0))
        .into()
    }

    fn view_unsorted(&self, kind: Option<OrgItem>) -> Element<'_, Msg> {
        let terms = &self.snap.unsorted;
        let heading = row![style::heading("Unsorted terms"), text(terms.len()).size(SIZE_SMALL).style(style::muted)]
            .spacing(SPACE[1])
            .align_y(Center);
        let mut block = column![heading].spacing(SPACE[0]);
        if terms.is_empty() {
            block = block.push(
                container(text("Nothing unsorted.").size(SIZE_SMALL).style(style::faint))
                    .padding(iced::padding::left(INDENT)),
            );
        }
        for t in terms {
            block = block.push(self.view_term(t, false, INDENT));
        }
        if matches!(kind, Some(OrgItem::Term(_))) { self.zone(block, OrgTarget::Unsorted) } else { block.into() }
    }

    fn view_ignored(&self, kind: Option<OrgItem>) -> Element<'_, Msg> {
        let toggle = button(
            row![
                text(format!("Ignored ({})", self.snap.ignored.len())).size(SIZE_BODY).font(style::SEMIBOLD),
                icon(if self.show_ignored { Icon::ChevronDown } else { Icon::ChevronRight }).size(SIZE_SMALL),
            ]
            .spacing(SPACE[0])
            .align_y(Center),
        )
        .padding(0)
        .style(style::row)
        .on_press(Msg::ToggleIgnored);
        let mut block = column![toggle].spacing(SPACE[0]);
        if self.show_ignored {
            for t in &self.snap.ignored {
                block = block.push(self.view_term(t, true, INDENT));
            }
        }
        if matches!(kind, Some(OrgItem::Term(_))) { self.zone(block, OrgTarget::Ignored) } else { block.into() }
    }
}

/// Whether `typed` is a real change of `name`.
fn renamed(name: &str, typed: &str) -> bool {
    !typed.trim().is_empty() && typed.trim() != name
}

/// The inline name field: Enter saves, Esc cancels.
fn name_input<'a>(placeholder: &'a str, value: &'a str) -> iced::widget::TextInput<'a, Msg> {
    text_input(placeholder, value)
        .id(NAME_ID)
        .style(style::text_input)
        .on_input(Msg::EditName)
        .on_submit(Msg::SaveName)
        .size(SIZE_BODY)
        .padding(SPACE[0])
        .width(260)
}

#[cfg(test)]
mod tests {
    use super::*;
    use juttusihti_core::store::Store;
    use juttusihti_core::{NewArticle, NewFeed, NewTerm};

    fn state() -> State {
        State::new(Db::new(Store::open_in_memory().unwrap()))
    }

    fn with<T>(st: &State, f: impl FnOnce(&mut Store) -> T) -> T {
        f(&mut st.db.shared().lock().unwrap())
    }

    /// Loads what the screen shows, as the runtime would after `load`.
    fn reload(st: &mut State) {
        let expanded = st.expanded.clone();
        let snap = with(st, |s| snapshot(s, &expanded)).map_err(|e| e.to_string());
        let _ = st.handle(Msg::Loaded(snap));
    }

    /// Sends `msg` and runs any store work it asks for synchronously.
    fn send(st: &mut State, msg: Msg) {
        match st.handle(msg) {
            Effect::Run(op) => {
                let r = with(st, |s| op.apply(s)).map_err(|e| e.to_string());
                let _ = st.handle(Msg::Done(r));
                reload(st);
            }
            Effect::Load => reload(st),
            Effect::None | Effect::Focus | Effect::Plan(_) | Effect::Suggest => {}
        }
    }

    fn drag_msgs(item: OrgItem, target: OrgTarget) -> Vec<Msg> {
        let nan = Point::new(f32::NAN, f32::NAN);
        vec![
            Msg::Drag(DragMsg::Press(item, "x".into(), nan)),
            Msg::Drag(DragMsg::Move(Point::new(0.0, 0.0))),
            Msg::Drag(DragMsg::Move(Point::new(40.0, 0.0))),
            Msg::Drag(DragMsg::Enter(target)),
            Msg::Drag(DragMsg::Release),
        ]
    }

    fn drag(st: &mut State, item: OrgItem, target: OrgTarget) {
        for m in drag_msgs(item, target) {
            send(st, m);
        }
    }

    /// The store work a drag would do, without doing it.
    fn drag_op(st: &mut State, item: OrgItem, target: OrgTarget) -> Option<Op> {
        drag_msgs(item, target).into_iter().map(|m| st.handle(m)).fold(None, |acc, e| match e {
            Effect::Run(op) => Some(op),
            _ => acc,
        })
    }

    /// One feed with an article carrying the publisher terms `terms`.
    fn add_terms(st: &State, terms: &[&str]) {
        with(st, |s| {
            let f = s
                .add_feed(&NewFeed {
                    url: "https://a.example/rss".into(),
                    title: "Example".into(),
                    ..Default::default()
                })
                .unwrap();
            let a = NewArticle {
                guid: "1".into(),
                url: Some("https://a.example/1".into()),
                title: "Title".into(),
                summary: None,
                content: None,
                author: None,
                published_at: Some(1_790_812_800),
                terms: terms.iter().map(|t| NewTerm { name: (*t).into(), scheme: String::new() }).collect(),
            };
            s.ingest(f, &[a]).unwrap();
        });
    }

    fn term_id(st: &State, name: &str) -> TermId {
        let snap = &st.snap;
        snap.unsorted.iter().chain(&snap.ignored).find(|t| t.name == name).unwrap().id
    }

    #[test]
    fn dropping_category_on_group_moves_it() {
        let mut st = state();
        let (ai, space, tech) = with(&st, |s| {
            (s.create_label("AI").unwrap(), s.create_label("Space").unwrap(), s.create_group("Tech").unwrap())
        });
        reload(&mut st);
        drag(&mut st, OrgItem::Category(ai), OrgTarget::GroupSlot(Some(tech), 0));
        drag(&mut st, OrgItem::Category(space), OrgTarget::GroupSlot(Some(tech), 0));
        let tree = with(&st, |s| s.category_tree()).unwrap();
        let names: Vec<&str> = tree.groups[0].categories.iter().map(|c| c.label.name.as_str()).collect();
        assert_eq!(names, ["Space", "AI"]);
        assert!(tree.ungrouped.is_empty());
        assert_eq!(st.snap.groups[0].categories.len(), 2, "the screen reloads after a drop");

        // Dropping a row below itself in the same group lands it after the row it was dropped on.
        drag(&mut st, OrgItem::Category(space), OrgTarget::GroupSlot(Some(tech), 2));
        let tree = with(&st, |s| s.category_tree()).unwrap();
        let names: Vec<&str> = tree.groups[0].categories.iter().map(|c| c.label.name.as_str()).collect();
        assert_eq!(names, ["AI", "Space"]);

        // Back to Ungrouped.
        drag(&mut st, OrgItem::Category(ai), OrgTarget::GroupSlot(None, 0));
        let tree = with(&st, |s| s.category_tree()).unwrap();
        assert_eq!(tree.ungrouped[0].label.id, ai);
    }

    #[test]
    fn dropping_term_on_category_maps_it() {
        let mut st = state();
        add_terms(&st, &["Tekoäly"]);
        let ai = with(&st, |s| s.create_label("AI")).unwrap();
        reload(&mut st);
        let t = term_id(&st, "Tekoäly");
        drag(&mut st, OrgItem::Term(t), OrgTarget::Category(ai));
        assert!(st.snap.unsorted.is_empty());
        assert_eq!(st.snap.ungrouped[0].term_count, 1);
        assert_eq!(with(&st, |s| s.category_terms(ai)).unwrap()[0].id, t);

        // Expanding the category lists the term with its feed.
        send(&mut st, Msg::ToggleTerms(ai));
        assert_eq!(st.snap.terms[&ai][0].feed_title, "Example");

        // And back to Unsorted.
        drag(&mut st, OrgItem::Term(t), OrgTarget::Unsorted);
        assert_eq!(st.snap.unsorted.len(), 1);
        assert_eq!(st.snap.ungrouped[0].term_count, 0);
    }

    #[test]
    fn ignore_and_unignore_term() {
        let mut st = state();
        add_terms(&st, &["Sää", "Urheilu"]);
        reload(&mut st);
        let t = term_id(&st, "Sää");
        send(&mut st, Msg::Ignore(t, true));
        assert_eq!(st.snap.ignored.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Sää"]);
        assert_eq!(st.snap.unsorted.len(), 1);
        send(&mut st, Msg::Ignore(t, false));
        assert!(st.snap.ignored.is_empty());
        assert_eq!(st.snap.unsorted.len(), 2);

        // Dropping on the Ignored section ignores too.
        let u = term_id(&st, "Urheilu");
        drag(&mut st, OrgItem::Term(u), OrgTarget::Ignored);
        assert_eq!(st.snap.ignored[0].id, u);
    }

    #[test]
    fn delete_group_needs_confirmation() {
        let mut st = state();
        let (ai, tech) = with(&st, |s| (s.create_label("AI").unwrap(), s.create_group("Tech").unwrap()));
        with(&st, |s| s.move_category(ai, Some(tech), 0)).unwrap();
        reload(&mut st);

        send(&mut st, Msg::AskDelete(Confirm::Group(tech)));
        assert_eq!(st.snap.groups.len(), 1, "asking deletes nothing");
        assert_eq!(st.confirm_text().as_deref(), Some("Delete group “Tech”? Its categories become Ungrouped."));
        send(&mut st, Msg::Escape);
        assert_eq!(st.confirm, None);
        send(&mut st, Msg::ConfirmDelete);
        assert_eq!(st.snap.groups.len(), 1, "confirming after Esc does nothing");

        send(&mut st, Msg::AskDelete(Confirm::Group(tech)));
        send(&mut st, Msg::ConfirmDelete);
        assert!(st.snap.groups.is_empty());
        assert_eq!(st.snap.ungrouped[0].label.id, ai, "its categories become Ungrouped");
        assert_eq!(st.confirm, None);
    }

    #[test]
    fn move_to_menu_matches_drag() {
        let mut st = state();
        add_terms(&st, &["Tekoäly"]);
        let (ai, space, tech, sci) = with(&st, |s| {
            (
                s.create_label("AI").unwrap(),
                s.create_label("Space").unwrap(),
                s.create_group("Tech").unwrap(),
                s.create_group("Science").unwrap(),
            )
        });
        with(&st, |s| s.move_category(space, Some(tech), 0)).unwrap();
        reload(&mut st);
        let t = term_id(&st, "Tekoäly");

        // A category moved to a group lands at its end, as when dropped on the group.
        let dests = st.dests(OrgItem::Category(ai));
        let to_tech = dests.iter().find(|d| d.1 == "Tech").unwrap().clone();
        assert!(!dests.iter().any(|d| d.1 == "Ungrouped"), "not offered where it already is");
        let by_menu = st.handle(Msg::MoveTo(OrgItem::Category(ai), to_tech));
        let by_drag = drag_op(&mut st, OrgItem::Category(ai), OrgTarget::GroupSlot(Some(tech), 1));
        assert_eq!(Some(Op::MoveCategory(ai, Some(tech), 1)), by_drag);
        assert!(matches!(by_menu, Effect::Run(op) if Some(&op) == by_drag.as_ref()));

        // A term moved to a category, or ignored.
        let dests = st.dests(OrgItem::Term(t));
        for (name, target) in [("AI", OrgTarget::Category(ai)), ("Ignored", OrgTarget::Ignored)] {
            let d = dests.iter().find(|d| d.1 == name).unwrap().clone();
            let by_menu = st.handle(Msg::MoveTo(OrgItem::Term(t), d));
            let by_drag = drag_op(&mut st, OrgItem::Term(t), target);
            assert!(by_drag.is_some());
            assert!(matches!(by_menu, Effect::Run(op) if Some(&op) == by_drag.as_ref()), "{name}");
        }

        // A group moved before another group.
        let d = st.dests(OrgItem::Group(sci)).into_iter().find(|d| d.1 == "Before “Tech”").unwrap();
        let by_menu = st.handle(Msg::MoveTo(OrgItem::Group(sci), d));
        let by_drag = drag_op(&mut st, OrgItem::Group(sci), OrgTarget::GroupSlot(Some(tech), 0));
        assert_eq!(by_drag, Some(Op::MoveGroup(sci, 0)));
        assert!(matches!(by_menu, Effect::Run(op) if Some(&op) == by_drag.as_ref()));

        // Reordering within a group: Space is first in Tech, AI joins after it.
        let first = st.dests(OrgItem::Category(ai))[0].clone();
        assert_eq!(first.1, "Tech");
        send(&mut st, Msg::MoveTo(OrgItem::Category(ai), first));
        let dests = st.dests(OrgItem::Category(ai));
        assert!(!dests.iter().any(|d| d.1 == "Before “AI”" || d.1 == "To the end of “Tech”"), "no-op moves");
        let d = dests.iter().find(|d| d.1 == "Before “Space”").unwrap().clone();
        let by_menu = st.handle(Msg::MoveTo(OrgItem::Category(ai), d));
        let by_drag = drag_op(&mut st, OrgItem::Category(ai), OrgTarget::GroupSlot(Some(tech), 0));
        assert_eq!(by_drag, Some(Op::MoveCategory(ai, Some(tech), 0)));
        assert!(matches!(by_menu, Effect::Run(op) if Some(&op) == by_drag.as_ref()));
        let d = st.dests(OrgItem::Category(space)).into_iter().find(|d| d.1 == "To the end of “Tech”").unwrap();
        assert_eq!(
            st.handle(Msg::MoveTo(OrgItem::Category(space), d)),
            Effect::Run(Op::MoveCategory(space, Some(tech), 1))
        );
        let d = st.dests(OrgItem::Category(ai)).into_iter().find(|d| d.1 == "Before “Space”").unwrap();
        send(&mut st, Msg::MoveTo(OrgItem::Category(ai), d));
        let tree = with(&st, |s| s.category_tree()).unwrap();
        let names: Vec<&str> = tree.groups[0].categories.iter().map(|c| c.label.name.as_str()).collect();
        assert_eq!(names, ["AI", "Space"]);
    }

    #[test]
    fn new_category_is_created_ungrouped_with_similar_name_warning() {
        let mut st = state();
        with(&st, |s| s.create_label("Urheilu")).unwrap();
        reload(&mut st);
        send(&mut st, Msg::NewCategory);
        send(&mut st, Msg::Escape);
        assert_eq!(st.editing, None, "Esc cancels");
        send(&mut st, Msg::NewCategory);
        send(&mut st, Msg::EditName("  Urheilua ".into()));
        send(&mut st, Msg::SaveName);
        assert_eq!(st.editing, None);
        let names: Vec<&str> = st.snap.ungrouped.iter().map(|c| c.label.name.as_str()).collect();
        assert_eq!(names, ["Urheilu", "Urheilua"]);
        assert_eq!(st.message, Some(Ok("Created “Urheilua”. Similar existing category: “Urheilu”.".to_string())));
        send(&mut st, Msg::NewCategory);
        send(&mut st, Msg::EditName("Talous".into()));
        send(&mut st, Msg::SaveName);
        assert_eq!(st.message, Some(Ok("Created “Talous”.".to_string())));
    }

    #[test]
    fn move_to_moves() {
        let mut st = state();
        let (ai, _tech) = with(&st, |s| (s.create_label("AI").unwrap(), s.create_group("Tech").unwrap()));
        reload(&mut st);
        // And it really moves.
        let first = st.dests(OrgItem::Category(ai))[0].clone();
        send(&mut st, Msg::MoveTo(OrgItem::Category(ai), first));
        assert_eq!(with(&st, |s| s.category_tree()).unwrap().groups[0].categories.len(), 1);
    }

    // ---- AI suggestions ------------------------------------------------

    use juttusihti_ai::categorize::MAX_TERMS;
    use juttusihti_core::ai_defaults::AiDefaults;
    use juttusihti_core::category_draft::{CategoryRef, GroupRef, Mapping, NewCategoryDraft, NewGroupDraft, Placement};
    use juttusihti_core::pricing::{Billing, Price, Provider};

    use crate::organise_draft::{DItem, DTarget, Name};

    /// A plan on a ChatGPT plan (a known, zero price) for what the store would send now.
    fn suggest_plan(st: &State, ready: Result<(), String>) -> Box<SuggestPlan> {
        let d = AiDefaults { billing: Billing::ChatGptPlan, ..Default::default() };
        let input = with(st, |s| s.category_plan_input(MAX_TERMS)).unwrap();
        Box::new(SuggestPlan::build(&d, input, Some(Price::chatgpt_plan(&d.model())), ready))
    }

    fn pending(st: &State) -> Option<(i64, CategoryDraft)> {
        with(st, |s| s.pending_category_draft()).unwrap()
    }

    /// Stores `draft` as the pending AI draft and reloads the screen.
    fn open_draft(st: &mut State, draft: &CategoryDraft) -> i64 {
        let id = with(st, |s| s.save_category_draft(draft, Provider::Openai, "m")).unwrap();
        reload(st);
        id
    }

    fn draft_msg(st: &mut State, m: DraftMsg) {
        send(st, Msg::Draft(m));
    }

    fn draft_drag(st: &mut State, item: DItem, target: DTarget) {
        let nan = Point::new(f32::NAN, f32::NAN);
        for m in [
            DragMsg::Press(item, "x".into(), nan),
            DragMsg::Move(Point::new(0.0, 0.0)),
            DragMsg::Move(Point::new(40.0, 0.0)),
            DragMsg::Enter(target),
            DragMsg::Release,
        ] {
            draft_msg(st, DraftMsg::Drag(m));
        }
    }

    #[test]
    fn consent_sends_nothing_until_send() {
        let mut st = state();
        add_terms(&st, &["Tekoäly", "Avaruus"]);
        reload(&mut st);

        assert_eq!(st.handle(Msg::Suggest), Effect::Plan(st.generation), "opening only estimates");
        assert!(matches!(st.suggest, Some(Panel::Planning)));
        let plan = suggest_plan(&st, Ok(()));
        assert_eq!(
            plan.sends_line(),
            "Sends 2 publisher terms with their feed names, and your category and group names, to OpenAI. No article text."
        );
        assert_eq!(plan.blocker(), None);
        assert_eq!(st.handle(Msg::Planned(st.generation, Ok(plan.clone()))), Effect::None);
        assert!(matches!(st.suggest, Some(Panel::Ready(_))));
        let _ = st.view();
        assert_eq!(st.handle(Msg::CloseSuggest), Effect::None);
        assert!(st.suggest.is_none(), "Cancel closes the panel");
        assert!(pending(&st).is_none(), "nothing was sent or saved");

        // A late estimate for the closed panel is dropped.
        let old = st.generation;
        let _ = st.handle(Msg::Suggest);
        assert_eq!(st.handle(Msg::Planned(old, Ok(plan.clone()))), Effect::None);
        assert!(matches!(st.suggest, Some(Panel::Planning)));

        // Not ready: Send does nothing.
        let not_ready = suggest_plan(&st, Err("Add an API key under AI setup.".into()));
        assert_eq!(not_ready.blocker().as_deref(), Some("Add an API key under AI setup."));
        let _ = st.handle(Msg::Planned(st.generation, Ok(not_ready)));
        assert_eq!(st.handle(Msg::SendSuggest), Effect::None);
        assert!(matches!(st.suggest, Some(Panel::Ready(_))));

        // Ready: Send is the one thing that sends.
        let _ = st.handle(Msg::Planned(st.generation, Ok(plan)));
        assert!(matches!(st.suggest, Some(Panel::Ready(_))), "a newer estimate replaces the shown one");
        assert_eq!(st.handle(Msg::SendSuggest), Effect::Suggest);
        assert!(matches!(st.suggest, Some(Panel::Running(_))));
        assert_eq!(st.handle(Msg::Suggest), Effect::None, "no second run while one is going");

        // A failure shows in the panel and saves nothing.
        let _ = st.handle(Msg::Suggested(Err("the provider declined".into())));
        assert!(matches!(&st.suggest, Some(Panel::Failed(e)) if e == "the provider declined"));
        assert!(st.draft.is_none() && pending(&st).is_none());
    }

    #[test]
    fn nothing_to_sort_disables_send() {
        let st = state();
        let plan = suggest_plan(&st, Ok(()));
        assert_eq!(plan.blocker().as_deref(), Some("Nothing is unsorted or ungrouped."));
        let tight = SuggestPlan {
            limits: juttusihti_core::budget::Limits { max_tokens: Some(10), ..plan.limits },
            input: with(&st, |s| {
                s.create_label("AI").unwrap();
                s.category_plan_input(MAX_TERMS)
            })
            .unwrap(),
            ..(*plan).clone()
        };
        assert!(tight.blocker().unwrap().contains("more than the per-run limit"));
        assert_eq!(tight.limit_line(), "Limit 10 tokens per run. Your ChatGPT plan's usage limits apply.");
    }

    #[test]
    fn pending_draft_reopens_in_draft_mode() {
        let mut st = state();
        add_terms(&st, &["Tekoäly"]);
        reload(&mut st);
        assert!(st.draft.is_none());
        let t = term_id(&st, "Tekoäly");
        let draft = CategoryDraft { vague: vec![t], ..Default::default() };
        let id = open_draft(&mut st, &draft);
        let d = st.draft.as_ref().expect("the pending draft opens on load");
        assert_eq!((d.id, &d.plan), (id, &draft));
        let _ = st.view();

        // A run's answer opens too, with the terms left for later.
        let mut st = state();
        let id = with(&st, |s| s.save_category_draft(&draft, Provider::Openai, "m")).unwrap();
        st.suggest = Some(Panel::Failed(String::new()));
        assert_eq!(st.handle(Msg::Suggested(Ok((id, Box::new(draft.clone()), 3)))), Effect::Load);
        assert!(st.suggest.is_none());
        reload(&mut st);
        assert_eq!(st.draft.as_ref().map(|d| (d.id, d.more_terms)), Some((id, 3)));

        // Cancel discards it and normal organising returns.
        draft_msg(&mut st, DraftMsg::Cancel);
        assert!(st.draft.is_none() && pending(&st).is_none());
    }

    /// A draft with AI group 1 holding new category 1 (with a term) and a placement.
    fn ai_draft(st: &State) -> (CategoryDraft, LabelId, TermId) {
        let space = with(st, |s| s.create_label("Space")).unwrap();
        let t = term_id(st, "Tekoäly");
        let draft = CategoryDraft {
            new_groups: vec![NewGroupDraft { temp: 1, name: "Tech".into(), by_user: false, ticked: true }],
            new_categories: vec![NewCategoryDraft {
                temp: 1,
                name: "AI".into(),
                group: GroupRef::New(1),
                ticked: true,
            }],
            placements: vec![Placement { label: space, rename: None, group: GroupRef::New(1), ticked: true }],
            mappings: vec![Mapping { term: t, category: CategoryRef::New(1), ticked: true }],
            vague: vec![],
        };
        (draft, space, t)
    }

    #[test]
    fn unticking_group_unticks_contents() {
        let mut st = state();
        add_terms(&st, &["Tekoäly"]);
        reload(&mut st);
        let (draft, space, _) = ai_draft(&st);
        open_draft(&mut st, &draft);

        draft_msg(&mut st, DraftMsg::Tick(Name::Group(1)));
        let plan = &st.draft.as_ref().unwrap().plan;
        assert!(!plan.new_groups[0].ticked);
        assert!(!plan.new_categories[0].ticked && !plan.placements[0].ticked, "its contents are unticked");
        assert_eq!(pending(&st).unwrap().1, *plan, "every edit is stored");

        // Ticking a category in an unticked group ticks the group again.
        draft_msg(&mut st, DraftMsg::Tick(Name::Category(CategoryRef::Existing(space))));
        let plan = &st.draft.as_ref().unwrap().plan;
        assert!(plan.new_groups[0].ticked && plan.placements[0].ticked);
        assert!(!plan.new_categories[0].ticked);
        assert_eq!(pending(&st).unwrap().1, *plan);
    }

    #[test]
    fn dragging_category_into_user_group_updates_draft() {
        let mut st = state();
        add_terms(&st, &["Tekoäly"]);
        let work = with(&st, |s| s.create_group("Work")).unwrap();
        reload(&mut st);
        let (draft, space, t) = ai_draft(&st);
        open_draft(&mut st, &draft);

        // + New group adds the user's own group.
        draft_msg(&mut st, DraftMsg::NewGroup);
        draft_msg(&mut st, DraftMsg::EditName("  Mine ".into()));
        draft_msg(&mut st, DraftMsg::SaveName);
        let plan = &st.draft.as_ref().unwrap().plan;
        let mine = plan.new_groups.iter().find(|g| g.by_user).expect("a user group");
        assert_eq!((mine.name.as_str(), mine.ticked), ("Mine", true));
        let mine = mine.temp;
        assert_ne!(mine, 1);

        draft_drag(&mut st, DItem::Category(CategoryRef::New(1)), DTarget::Group(GroupRef::New(mine)));
        draft_drag(&mut st, DItem::Category(CategoryRef::Existing(space)), DTarget::Group(GroupRef::Existing(work)));
        let plan = &st.draft.as_ref().unwrap().plan;
        assert_eq!(plan.new_categories[0].group, GroupRef::New(mine));
        assert_eq!(plan.placements[0].group, GroupRef::Existing(work));

        // Terms move between categories; placements are renamed by clicking.
        draft_drag(&mut st, DItem::Term(t), DTarget::Category(CategoryRef::Existing(space)));
        draft_msg(&mut st, DraftMsg::StartRename(Name::Category(CategoryRef::Existing(space))));
        draft_msg(&mut st, DraftMsg::EditName("Space travel".into()));
        draft_msg(&mut st, DraftMsg::SaveName);
        let plan = st.draft.as_ref().unwrap().plan.clone();
        assert_eq!(plan.mappings[0].category, CategoryRef::Existing(space));
        assert_eq!(plan.placements[0].rename.as_deref(), Some("Space travel"));
        assert_eq!(pending(&st).unwrap().1, plan, "every edit is stored");
        let _ = st.view();

        // Renaming back to the original name drops the rename.
        draft_msg(&mut st, DraftMsg::StartRename(Name::Category(CategoryRef::Existing(space))));
        draft_msg(&mut st, DraftMsg::EditName("Space".into()));
        draft_msg(&mut st, DraftMsg::SaveName);
        assert_eq!(st.draft.as_ref().unwrap().plan.placements[0].rename, None);
    }

    #[test]
    fn approve_reports_skipped() {
        let mut st = state();
        add_terms(&st, &["Tekoäly", "Avaruus"]);
        reload(&mut st);
        let ai = with(&st, |s| s.create_label("AI")).unwrap();
        let (a, b) = (term_id(&st, "Tekoäly"), term_id(&st, "Avaruus"));
        let draft = CategoryDraft {
            mappings: vec![
                Mapping { term: a, category: CategoryRef::Existing(ai), ticked: true },
                Mapping { term: b, category: CategoryRef::Existing(ai), ticked: true },
            ],
            vague: vec![],
            ..Default::default()
        };
        open_draft(&mut st, &draft);
        // The user ignores one term meanwhile.
        with(&st, |s| s.set_term_ignored(b, true)).unwrap();

        draft_msg(&mut st, DraftMsg::Approve);
        assert_eq!(st.message, Some(Ok("Applied 1 change, skipped 1 that changed meanwhile.".to_string())));
        assert!(st.draft.is_none(), "normal organising returns");
        assert!(pending(&st).is_none());
        assert_eq!(with(&st, |s| s.category_terms(ai)).unwrap()[0].id, a);

        use juttusihti_core::category_draft::ApplyReport;
        assert_eq!(ai::applied_text(ApplyReport { applied: 3, skipped: 0 }), "Applied 3 changes.");
    }

    #[test]
    fn draft_copy() {
        let mut st = state();
        add_terms(&st, &["Sää"]);
        reload(&mut st);
        let t = term_id(&st, "Sää");
        open_draft(&mut st, &CategoryDraft { vague: vec![t], ..Default::default() });
        assert_eq!(
            crate::organise_draft::vague_line(3).as_deref(),
            Some("3 terms were too vague to sort and stay unsorted.")
        );
        assert_eq!(crate::organise_draft::vague_line(0), None);
    }
}

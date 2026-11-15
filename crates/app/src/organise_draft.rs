//! AI category suggestions in the Organise screen: the consent panel (what
//! is sent, the estimate and the per-run limit) and the editable draft that
//! comes back. Nothing is sent before the panel's Send, and nothing in the
//! draft changes the categories before Approve.

use chrono::Local;
use iced::widget::{Space, button, checkbox, column, container, row, scrollable, text, text_input};
use iced::{Center, Element, Fill, Point};
use juttusihti_ai::categorize::{self, MAX_TERMS};
use juttusihti_ai::{Client, RetryPolicy};
use juttusihti_core::ai_defaults::AiDefaults;
use juttusihti_core::budget::Limits;
use juttusihti_core::categories::CategoryId;
use juttusihti_core::categories::{CategoryNode, TermId, TermRow};
use juttusihti_core::category_draft::{
    ApplyReport, CategoryDraft, CategoryRef, GroupRef, NewCategoryDraft, NewGroupDraft, PlanInput,
};
use juttusihti_core::pricing::{Billing, Catalog, Micros, Price, Provider, Usage, format_usd};

use crate::db::Db;
use crate::drag::{self, DragMsg, DragState};
use crate::icons::{Icon, icon};
use crate::organise::{Msg, NAME_ID, Snapshot};
use crate::research::{cost_text, thousands};
use crate::style::{self, SIZE_BODY, SIZE_SMALL, SPACE};
use crate::{ai_setup, research};

/// Left indent of category rows under their group heading, and of terms under their category.
const INDENT: f32 = 22.0;

// ---- consent -------------------------------------------------------------

/// What a suggestion run would send and cost, worked out when the panel opens.
#[derive(Debug, Clone)]
pub struct SuggestPlan {
    pub provider: Provider,
    pub billing: Billing,
    pub model: String,
    /// Exactly what is sent.
    pub input: PlanInput,
    /// Worst-case tokens (input and output) and their cost.
    pub tokens: u64,
    pub cost: Option<Micros>,
    pub price: Option<Price>,
    pub limits: Limits,
    pub ready: Result<(), String>,
}

impl SuggestPlan {
    pub fn build(d: &AiDefaults, input: PlanInput, price: Option<Price>, ready: Result<(), String>) -> Self {
        let (worst_in, worst_out) = categorize::estimate_tokens(&input);
        let worst = Usage { input: worst_in, output: worst_out, ..Default::default() };
        Self {
            provider: d.provider,
            billing: d.billing(),
            model: d.model(),
            tokens: worst.total(),
            cost: price.as_ref().and_then(|p| p.cost(&worst, false)),
            price,
            input,
            limits: run_limits(d),
            ready,
        }
    }

    /// Why Send is disabled, if it is.
    pub fn blocker(&self) -> Option<String> {
        if self.input.terms.is_empty() && self.input.categories.iter().all(|c| c.group_id.is_some()) {
            return Some("Nothing is unsorted or ungrouped.".into());
        }
        if let Err(e) = &self.ready {
            return Some(e.clone());
        }
        over_limit(&self.limits, self.tokens, self.cost, &self.model)
    }

    /// What is sent, and to whom.
    pub fn sends_line(&self) -> String {
        let terms = match self.input.terms.len() {
            1 => "1 publisher term with its feed name".to_string(),
            n => format!("{n} publisher terms with their feed names"),
        };
        format!("Sends {terms}, and your category and group names, to {}. No article text.", self.provider)
    }

    pub fn estimate_line(&self) -> String {
        format!("Estimate: at most {} tokens, {}", thousands(self.tokens), cost_text(Some(self.billing), self.cost))
    }

    pub fn limit_line(&self) -> String {
        limit_line(self.billing, &self.limits)
    }
}

/// The per-run limits of a category run, as the quick analysis: the user's
/// token and cost limits, and a known price.
pub(crate) fn run_limits(d: &AiDefaults) -> Limits {
    Limits { max_tokens: d.max_tokens, max_cost: d.max_cost, max_calls: None, allow_unknown_cost: false }
}

/// Why a run of at most `tokens` costing `cost` may not be sent under `limits`.
pub(crate) fn over_limit(limits: &Limits, tokens: u64, cost: Option<Micros>, model: &str) -> Option<String> {
    if limits.max_tokens.is_some_and(|max| tokens > max) {
        return Some(format!("This could use up to {} tokens, more than the per-run limit.", thousands(tokens)));
    }
    match (limits.max_cost, cost) {
        (Some(_), None) => Some(format!("The price of {model} is unknown, so its cost cannot be limited.")),
        (Some(max), Some(c)) if c > max => {
            Some(format!("This could cost up to {}, more than the per-run limit.", format_usd(c)))
        }
        _ => None,
    }
}

pub(crate) fn limit_line(billing: Billing, limits: &Limits) -> String {
    let mut parts = Vec::new();
    if billing == Billing::ApiKey
        && let Some(c) = limits.max_cost
    {
        parts.push(format_usd(c));
    }
    if let Some(t) = limits.max_tokens {
        parts.push(format!("{} tokens", thousands(t)));
    }
    let limit = if parts.is_empty() {
        "No per-run limit.".to_string()
    } else {
        format!("Limit {} per run.", parts.join(" and "))
    };
    match billing {
        Billing::ChatGptPlan => format!("{limit} Your ChatGPT plan's usage limits apply."),
        Billing::ApiKey => limit,
    }
}

/// The consent panel's state.
#[derive(Debug)]
pub enum Panel {
    Planning,
    Ready(Box<SuggestPlan>),
    /// Sent; waiting for the answer.
    Running(Box<SuggestPlan>),
    Failed(String),
}

/// Works out what a run would send and cost. Sends nothing.
pub async fn plan(db: Db, catalog: Catalog) -> Result<Box<SuggestPlan>, String> {
    let mut d = ai_setup::defaults(&db).await?;
    // The clickbait check plays no part here, so its key is not needed.
    d.clickbait.enabled = false;
    let ready = ai_setup::readiness(&d).await;
    let input = db.call(|s| s.category_plan_input(MAX_TERMS)).await?;
    let price = catalog.price_for(d.provider, &d.model(), d.billing(), Local::now().date_naive());
    Ok(Box::new(SuggestPlan::build(&d, input, price, ready)))
}

/// Shown instead of an empty draft when the AI proposed nothing.
pub(crate) const NO_SUGGESTIONS: &str = "The AI suggested no changes. Nothing was saved.";

/// Shown when the AI's grouping put nearly every category into one group.
pub(crate) const GROUPING_SKIPPED: &str = "The AI put nearly all categories into one group, so no grouping is suggested. Create your own groups, or try again.";

/// Why nothing was saved for an empty `draft`.
pub(crate) fn empty_reason(draft: &CategoryDraft) -> &'static str {
    if draft.grouping_skipped { GROUPING_SKIPPED } else { NO_SUGGESTIONS }
}

/// Sends the consented request and saves the validated answer as the pending
/// draft: its id, the draft and how many terms wait for a later run.
pub async fn run(db: Db, plan: SuggestPlan) -> Result<(i64, Box<CategoryDraft>, usize), String> {
    let endpoint = research::endpoint(plan.provider, plan.billing).await?;
    let client = Client::new(endpoint, RetryPolicy::default()).map_err(|e| e.to_string())?;
    let draft = categorize::suggest(&client, &plan.model, &plan.input, plan.price.as_ref(), plan.limits, db.shared())
        .await
        .map_err(|e| e.to_string())?;
    if draft.is_empty() {
        return Err(empty_reason(&draft).into());
    }
    let (saved, provider, model) = (draft.clone(), plan.provider, plan.model.clone());
    let id = db.call(move |s| s.save_category_draft(&saved, provider, &model)).await?;
    Ok((id, Box::new(draft), plan.input.more_terms))
}

/// The panel under the header: estimating, consent, progress or the error.
pub fn view_panel(panel: &Panel) -> Element<'_, Msg> {
    let close = || style::quiet_button(None, "Close", style::quiet, Some(Msg::CloseSuggest));
    let content: Element<'_, Msg> = match panel {
        Panel::Planning => style::note("Estimating…").into(),
        Panel::Running(p) => style::note(format!("Asking {} for suggestions…", p.provider)).into(),
        Panel::Failed(e) => row![text(e).size(SIZE_SMALL).style(style::danger).width(Fill), close()]
            .spacing(SPACE[1])
            .align_y(Center)
            .into(),
        Panel::Ready(p) => {
            let small = |line: String| text(line).size(SIZE_SMALL).style(style::body);
            let plan = if p.billing == Billing::ChatGptPlan { " on your ChatGPT plan" } else { "" };
            let mut col = column![
                style::heading("Suggest categories"),
                style::note(format!("{} · {}{plan}", p.provider, p.model)),
                small(p.sends_line()),
                small(p.estimate_line()),
                style::note(p.limit_line()),
            ]
            .spacing(SPACE[1]);
            let blocker = p.blocker();
            if let Some(why) = &blocker {
                col = col.push(text(why.clone()).size(SIZE_SMALL).style(style::danger));
            }
            col.push(
                row![
                    style::main_button("Send", blocker.is_none().then_some(Msg::SendSuggest)),
                    style::quiet_button(None, "Cancel", style::quiet, Some(Msg::CloseSuggest)),
                ]
                .spacing(SPACE[1])
                .align_y(Center),
            )
            .into()
        }
    };
    container(content).padding(SPACE[3]).width(Fill).style(style::card).into()
}

/// The message after Approve.
pub fn applied_text(r: ApplyReport) -> String {
    let changes = if r.applied == 1 { "change" } else { "changes" };
    let skipped = if r.skipped > 0 { format!(", skipped {} that changed meanwhile", r.skipped) } else { String::new() };
    format!("Applied {} {changes}{skipped}.", r.applied)
}

// ---- draft ---------------------------------------------------------------

/// A draft row that can be dragged: a new category or placement, or a term.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DItem {
    /// `New`: a suggested category; `Existing`: a placement of an ungrouped one.
    Category(CategoryRef),
    Term(TermId),
}

/// Where a draft row can land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DTarget {
    Group(GroupRef),
    Category(CategoryRef),
}

/// A tickable, renamable draft entry: a new group, or a new category or placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Name {
    Group(u32),
    Category(CategoryRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Edit {
    NewGroup(String),
    Rename(Name, String),
}

#[derive(Debug, Clone)]
pub enum DraftMsg {
    Tick(Name),
    /// Ticks or unticks one suggested term mapping.
    TickTerm(TermId),
    StartRename(Name),
    EditName(String),
    SaveName,
    NewGroup,
    Escape,
    Drag(DragMsg<DItem, DTarget>),
    ToggleTerms(CategoryRef),
    Approve,
    Cancel,
}

/// What a draft message asks of the screen.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    None,
    /// The draft changed: store it.
    Changed,
    Focus,
    /// The draft changed (a name typed in the field that closed) and a new name field opened:
    /// store it and focus the field.
    ChangedAndFocus,
    Approve,
    Discard,
}

pub struct Draft {
    pub id: i64,
    pub plan: CategoryDraft,
    /// Terms left for a later run (known right after a run only).
    pub more_terms: usize,
    edit: Option<Edit>,
    drag: DragState<DItem, DTarget>,
    expanded: Vec<CategoryRef>,
    /// Approve or Cancel was pressed: the draft takes no more edits.
    closing: bool,
    /// A save is in flight; only one runs at a time.
    saving: bool,
    /// The draft changed while a save was in flight.
    dirty: bool,
}

impl Draft {
    pub fn new(id: i64, plan: CategoryDraft, more_terms: usize) -> Self {
        Self {
            id,
            plan,
            more_terms,
            edit: None,
            drag: DragState::default(),
            expanded: Vec::new(),
            closing: false,
            saving: false,
            dirty: false,
        }
    }

    /// Whether Approve or Cancel is running.
    pub fn closing(&self) -> bool {
        self.closing
    }

    /// Approve or Cancel failed: the draft can be edited again.
    pub fn reopen(&mut self) {
        self.closing = false;
    }

    /// A save returned. True when the draft changed meanwhile and must be
    /// saved again (the caller sends the current plan).
    pub fn saved(&mut self) -> bool {
        self.saving = false;
        if self.dirty && !self.closing {
            self.dirty = false;
            self.saving = true;
            return true;
        }
        false
    }

    pub fn cancel_drag(&mut self) {
        self.drag.update(DragMsg::Cancel);
    }

    pub fn dragging(&self) -> bool {
        self.drag.dragging().is_some()
    }

    /// Whether Esc has something to close.
    pub fn busy(&self) -> bool {
        self.dragging() || self.edit.is_some()
    }

    pub fn ghost(&self) -> Option<(&str, Point)> {
        self.drag.dragging().filter(|d| d.active).map(|d| (d.label.as_str(), d.cursor))
    }

    pub fn update(&mut self, msg: DraftMsg, snap: &Snapshot) -> Outcome {
        if self.closing {
            return Outcome::None;
        }
        match self.apply(msg, snap) {
            Outcome::Changed if self.saving => {
                self.dirty = true;
                Outcome::None
            }
            Outcome::Changed => {
                self.saving = true;
                Outcome::Changed
            }
            Outcome::ChangedAndFocus if self.saving => {
                self.dirty = true;
                Outcome::Focus
            }
            Outcome::ChangedAndFocus => {
                self.saving = true;
                Outcome::ChangedAndFocus
            }
            o @ (Outcome::Approve | Outcome::Discard) => {
                self.closing = true;
                self.edit = None;
                self.drag.update(DragMsg::Cancel);
                o
            }
            o => o,
        }
    }

    /// Closes the name field and applies what it holds; true when the draft changed.
    fn save_typed(&mut self, snap: &Snapshot) -> bool {
        match self.edit.take() {
            Some(Edit::NewGroup(typed)) => self.add_group(typed.trim()),
            Some(Edit::Rename(n, typed)) => self.rename(n, typed.trim(), snap),
            None => false,
        }
    }

    fn apply(&mut self, msg: DraftMsg, snap: &Snapshot) -> Outcome {
        let changed = |c: bool| if c { Outcome::Changed } else { Outcome::None };
        match msg {
            DraftMsg::Tick(n) => {
                self.tick_entry(n);
                Outcome::Changed
            }
            DraftMsg::TickTerm(t) => match self.plan.mappings.iter_mut().find(|m| m.term == t) {
                Some(m) => {
                    m.ticked = !m.ticked;
                    Outcome::Changed
                }
                None => Outcome::None,
            },
            DraftMsg::StartRename(n) => {
                let saved = self.save_typed(snap);
                self.edit = self.shown_name(n, snap).map(|s| Edit::Rename(n, s));
                match (saved, self.edit.is_some()) {
                    (true, true) => Outcome::ChangedAndFocus,
                    (false, true) => Outcome::Focus,
                    (saved, false) => changed(saved),
                }
            }
            DraftMsg::EditName(s) => {
                if let Some(Edit::NewGroup(typed) | Edit::Rename(_, typed)) = &mut self.edit {
                    *typed = s;
                }
                Outcome::None
            }
            DraftMsg::SaveName => changed(self.save_typed(snap)),
            DraftMsg::NewGroup => {
                let saved = self.save_typed(snap);
                self.edit = Some(Edit::NewGroup(String::new()));
                if saved { Outcome::ChangedAndFocus } else { Outcome::Focus }
            }
            DraftMsg::Escape => {
                self.drag.update(DragMsg::Cancel);
                self.edit = None;
                Outcome::None
            }
            DraftMsg::Drag(m) => match self.drag.update(m) {
                Some((item, target)) => changed(self.drop(item, target)),
                None => Outcome::None,
            },
            DraftMsg::ToggleTerms(c) => {
                if let Some(i) = self.expanded.iter().position(|e| *e == c) {
                    self.expanded.remove(i);
                } else {
                    self.expanded.push(c);
                }
                Outcome::None
            }
            DraftMsg::Approve => {
                // A name still being typed is part of what is approved.
                if self.edit.is_some() {
                    let _ = self.apply(DraftMsg::SaveName, snap);
                }
                Outcome::Approve
            }
            DraftMsg::Cancel => Outcome::Discard,
        }
    }

    /// Applies dropping `item` on `target`; false when nothing changes.
    pub fn drop(&mut self, item: DItem, target: DTarget) -> bool {
        match (item, target) {
            (DItem::Category(c), DTarget::Group(g)) => match c {
                CategoryRef::New(t) => match self.plan.new_categories.iter_mut().find(|x| x.temp == t) {
                    Some(x) if x.group != Some(g) => {
                        x.group = Some(g);
                        true
                    }
                    _ => false,
                },
                CategoryRef::Existing(l) => match self.plan.placements.iter_mut().find(|p| p.category == l) {
                    Some(p) if p.group != g => {
                        p.group = g;
                        true
                    }
                    _ => false,
                },
            },
            (DItem::Term(t), DTarget::Category(c)) => match self.plan.mappings.iter_mut().find(|m| m.term == t) {
                Some(m) if m.category != c => {
                    m.category = c;
                    m.ticked = true;
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Ticks or unticks an entry. A group passes its new state on to its
    /// contents; ticking an entry of an unticked group ticks the group.
    fn tick_entry(&mut self, n: Name) {
        let on = !self.ticked(n);
        let group = match n {
            Name::Group(t) => {
                for g in self.plan.new_groups.iter_mut().filter(|g| g.temp == t) {
                    g.ticked = on;
                }
                for c in self.plan.new_categories.iter_mut().filter(|c| c.group == Some(GroupRef::New(t))) {
                    c.ticked = on;
                }
                for p in self.plan.placements.iter_mut().filter(|p| p.group == GroupRef::New(t)) {
                    p.ticked = on;
                }
                return;
            }
            Name::Category(CategoryRef::New(t)) => {
                self.plan.new_categories.iter_mut().find(|c| c.temp == t).and_then(|c| {
                    c.ticked = on;
                    c.group
                })
            }
            Name::Category(CategoryRef::Existing(l)) => {
                self.plan.placements.iter_mut().find(|p| p.category == l).map(|p| {
                    p.ticked = on;
                    p.group
                })
            }
        };
        if let (true, Some(GroupRef::New(t))) = (on, group) {
            for g in self.plan.new_groups.iter_mut().filter(|g| g.temp == t) {
                g.ticked = true;
            }
        }
    }

    /// The name an entry shows; `None` for anything not editable in the draft.
    fn shown_name(&self, n: Name, snap: &Snapshot) -> Option<String> {
        match n {
            Name::Group(t) => self.plan.new_groups.iter().find(|g| g.temp == t).map(|g| g.name.clone()),
            Name::Category(CategoryRef::New(t)) => {
                self.plan.new_categories.iter().find(|c| c.temp == t).map(|c| c.name.clone())
            }
            Name::Category(CategoryRef::Existing(l)) => {
                let p = self.plan.placements.iter().find(|p| p.category == l)?;
                p.rename.clone().or_else(|| category_name(snap, l).map(str::to_string))
            }
        }
    }

    /// Adds the user's own group (`by_user`), ticked.
    fn add_group(&mut self, name: &str) -> bool {
        if name.is_empty() {
            return false;
        }
        let temp = self.plan.new_groups.iter().map(|g| g.temp).max().map_or(1, |t| t.saturating_add(1));
        self.plan.new_groups.push(NewGroupDraft { temp, name: name.into(), by_user: true, ticked: true });
        true
    }

    fn rename(&mut self, n: Name, name: &str, snap: &Snapshot) -> bool {
        if name.is_empty() {
            return false;
        }
        let slot = match n {
            Name::Group(t) => self.plan.new_groups.iter_mut().find(|g| g.temp == t).map(|g| &mut g.name),
            Name::Category(CategoryRef::New(t)) => {
                self.plan.new_categories.iter_mut().find(|c| c.temp == t).map(|c| &mut c.name)
            }
            Name::Category(CategoryRef::Existing(l)) => {
                // A placement keeps a rename only while it differs from the category's name.
                let original = category_name(snap, l);
                let Some(p) = self.plan.placements.iter_mut().find(|p| p.category == l) else { return false };
                let rename = (Some(name) != original).then(|| name.to_string());
                if p.rename == rename {
                    return false;
                }
                p.rename = rename;
                return true;
            }
        };
        match slot {
            Some(slot) if slot != name => {
                *slot = name.into();
                true
            }
            _ => false,
        }
    }

    fn ticked(&self, n: Name) -> bool {
        match n {
            Name::Group(t) => self.plan.new_groups.iter().any(|g| g.temp == t && g.ticked),
            Name::Category(CategoryRef::New(t)) => self.plan.new_categories.iter().any(|c| c.temp == t && c.ticked),
            Name::Category(CategoryRef::Existing(l)) => {
                self.plan.placements.iter().any(|p| p.category == l && p.ticked)
            }
        }
    }

    /// The categories the draft puts into group `g`, as (reference, shown name).
    fn members<'a>(&'a self, g: GroupRef, snap: &'a Snapshot) -> Vec<(CategoryRef, &'a str)> {
        let new = self
            .plan
            .new_categories
            .iter()
            .filter(|c| c.group == Some(g))
            .map(|c| (CategoryRef::New(c.temp), c.name.as_str()));
        let placed = self.plan.placements.iter().filter(|p| p.group == g).filter_map(|p| {
            let name = p.rename.as_deref().or_else(|| category_name(snap, p.category))?;
            Some((CategoryRef::Existing(p.category), name))
        });
        placed.chain(new).collect()
    }

    /// The terms the draft maps to `c` that are still unsorted, with whether each is ticked.
    fn terms<'a>(&'a self, c: CategoryRef, snap: &'a Snapshot) -> Vec<(&'a TermRow, bool)> {
        self.plan
            .mappings
            .iter()
            .filter(|m| m.category == c)
            .filter_map(|m| snap.unsorted_at.get(&m.term).map(|&i| (&snap.unsorted[i], m.ticked)))
            .collect()
    }

    // ---- view ----------------------------------------------------------

    pub fn view<'a>(
        &'a self,
        snap: &'a Snapshot,
        message: Option<&'a Result<String, String>>,
    ) -> Element<'a, DraftMsg> {
        let kind = self.drag.dragging().map(|d| d.item);
        let mut body = column![].spacing(SPACE[4]);
        for g in &self.plan.new_groups {
            body = body.push(self.view_new_group(g, snap, kind));
        }
        for g in &snap.groups {
            let r = GroupRef::Existing(g.group.id);
            let heading = row![text(&g.group.name).size(SIZE_BODY).font(style::SEMIBOLD), yours()]
                .spacing(SPACE[1])
                .align_y(Center);
            let mut block = column![heading].spacing(SPACE[0]);
            for c in &g.categories {
                block =
                    block.push(self.view_category(CategoryRef::Existing(c.category.id), &c.category.name, snap, kind));
            }
            block = block.push(self.view_members(r, snap, kind, g.categories.is_empty()));
            body = body.push(self.group_zone(block, r, kind));
        }
        body = body.push(self.view_add_group());
        // The user's ungrouped categories the draft does not place (read-only term targets),
        // and the new categories the draft leaves without a group (drag one onto a group to file it).
        let own: Vec<&CategoryNode> = snap
            .ungrouped
            .iter()
            .filter(|c| !self.plan.placements.iter().any(|p| p.category == c.category.id))
            .collect();
        let new: Vec<&NewCategoryDraft> = self.plan.new_categories.iter().filter(|c| c.group.is_none()).collect();
        if !own.is_empty() || !new.is_empty() {
            let mut heading = row![style::heading("Ungrouped")].spacing(SPACE[1]).align_y(Center);
            if new.is_empty() {
                heading = heading.push(yours());
            }
            let mut block = column![heading].spacing(SPACE[0]);
            for c in own {
                block =
                    block.push(self.view_category(CategoryRef::Existing(c.category.id), &c.category.name, snap, kind));
            }
            for c in new {
                block = block.push(self.view_category(CategoryRef::New(c.temp), &c.name, snap, kind));
            }
            body = body.push(block);
        }
        if let Some(line) = vague_line(self.plan.vague.len()) {
            body = body.push(style::note(line));
        }
        if self.plan.grouping_skipped {
            body = body.push(style::note(GROUPING_SKIPPED));
        }

        let mut header = column![
            row![icon(Icon::Sparkles).size(SIZE_BODY).style(style::accent), style::title("Suggested categories")]
                .spacing(SPACE[1])
                .align_y(Center),
            style::note("Nothing is saved until you approve."),
        ]
        .spacing(SPACE[0]);
        if self.more_terms > 0 {
            header = header.push(style::note(more_terms_line(self.more_terms)));
        }
        let footer = row![
            Space::new().width(Fill),
            style::quiet_button(None, "Cancel", style::quiet, (!self.closing).then_some(DraftMsg::Cancel)),
            style::main_button("Approve", (!self.closing).then_some(DraftMsg::Approve)),
        ]
        .spacing(SPACE[1])
        .align_y(Center);
        let mut page = column![header].spacing(SPACE[3]).max_width(720.0);
        if let Some(m) = message {
            page = page.push(style::message(m));
        }
        page = page.push(scrollable(container(body).padding(iced::padding::right(SPACE[3]))).height(Fill)).push(footer);
        container(page).padding(SPACE[4]).width(Fill).height(Fill).into()
    }

    fn over(&self, t: DTarget) -> bool {
        self.drag.dragging().is_some_and(|d| d.active && d.over == Some(t))
    }

    fn zone<'a>(&self, content: impl Into<Element<'a, DraftMsg>>, t: DTarget) -> Element<'a, DraftMsg> {
        let (enter, leave) = (DraftMsg::Drag(DragMsg::Enter(t)), DraftMsg::Drag(DragMsg::Leave(t)));
        drag::target(content.into(), self.over(t), enter, leave)
    }

    fn group_zone<'a>(
        &self,
        block: iced::widget::Column<'a, DraftMsg>,
        g: GroupRef,
        kind: Option<DItem>,
    ) -> Element<'a, DraftMsg> {
        if matches!(kind, Some(DItem::Category(_))) { self.zone(block, DTarget::Group(g)) } else { block.into() }
    }

    fn grip<'a>(&self, item: DItem, label: &str, content: impl Into<Element<'a, DraftMsg>>) -> Element<'a, DraftMsg> {
        let label = label.to_string();
        drag::handle(
            content.into(),
            move |p| DraftMsg::Drag(DragMsg::Press(item, label.clone(), p)),
            DraftMsg::Drag(DragMsg::Release),
        )
    }

    /// A draft name: click to rename, or the open name field.
    fn name<'a>(&'a self, n: Name, name: &'a str, strong: bool) -> Element<'a, DraftMsg> {
        if let Some(Edit::Rename(e, typed)) = &self.edit
            && *e == n
        {
            return name_field("Name", typed);
        }
        let mut label = text(name).size(SIZE_BODY);
        if strong {
            label = label.font(style::SEMIBOLD);
        }
        if !self.ticked(n) {
            label = label.style(style::faint);
        }
        button(label).padding([0.0, SPACE[0]]).style(style::row).on_press(DraftMsg::StartRename(n)).into()
    }

    fn tick<'a>(&self, n: Name) -> Element<'a, DraftMsg> {
        checkbox(self.ticked(n)).size(SIZE_SMALL).style(style::checkbox).on_toggle(move |_| DraftMsg::Tick(n)).into()
    }

    fn view_new_group<'a>(
        &'a self,
        g: &'a NewGroupDraft,
        snap: &'a Snapshot,
        kind: Option<DItem>,
    ) -> Element<'a, DraftMsg> {
        let n = Name::Group(g.temp);
        let tag = if g.by_user { yours() } else { text("AI").size(SIZE_SMALL).style(style::accent) };
        let heading = row![self.tick(n), self.name(n, &g.name, true), tag].spacing(SPACE[1]).align_y(Center);
        let r = GroupRef::New(g.temp);
        let block = column![heading, self.view_members(r, snap, kind, true)].spacing(SPACE[0]);
        self.group_zone(block, r, kind)
    }

    /// The draft's categories in group `g`; `note_empty` shows a hint when there are none.
    fn view_members<'a>(
        &'a self,
        g: GroupRef,
        snap: &'a Snapshot,
        kind: Option<DItem>,
        note_empty: bool,
    ) -> Element<'a, DraftMsg> {
        let members = self.members(g, snap);
        let mut col = column![];
        if members.is_empty() && note_empty {
            col = col.push(
                container(text("Empty. Drag categories here.").size(SIZE_SMALL).style(style::faint))
                    .padding(iced::padding::left(INDENT).top(SPACE[0]).bottom(SPACE[0])),
            );
        }
        for (c, name) in members {
            col = col.push(self.view_category(c, name, snap, kind));
        }
        col.into()
    }

    /// A category row. Draft entries (new categories, placements) can be
    /// ticked, renamed and dragged; the user's own categories are read-only.
    fn view_category<'a>(
        &'a self,
        c: CategoryRef,
        name: &'a str,
        snap: &'a Snapshot,
        kind: Option<DItem>,
    ) -> Element<'a, DraftMsg> {
        let editable = match c {
            CategoryRef::New(_) => true,
            CategoryRef::Existing(l) => self.plan.placements.iter().any(|p| p.category == l),
        };
        let terms = self.terms(c, snap);
        let open = self.expanded.contains(&c);
        let mut line = row![].spacing(SPACE[1]).align_y(Center);
        if editable {
            let n = Name::Category(c);
            line = line.push(self.tick(n)).push(self.grip(DItem::Category(c), name, self.name(n, name, false)));
        } else {
            line = line.push(text(name).size(SIZE_BODY).style(style::body));
        }
        line = line.push(Space::new().width(Fill));
        if !terms.is_empty() {
            let n = terms.len();
            line = line.push(
                button(
                    row![
                        text(format!("{n} {}", if n == 1 { "term" } else { "terms" }))
                            .size(SIZE_SMALL)
                            .style(style::muted),
                        icon(if open { Icon::ChevronDown } else { Icon::ChevronRight })
                            .size(SIZE_SMALL)
                            .style(style::muted),
                    ]
                    .spacing(SPACE[0])
                    .align_y(Center),
                )
                .padding([0.0, SPACE[0]])
                .style(style::row)
                .on_press(DraftMsg::ToggleTerms(c)),
            );
        }
        let mut col = column![container(line).padding(iced::padding::left(INDENT).top(2.0).bottom(2.0))];
        if open {
            for (t, ticked) in terms {
                // An unticked term stays unsorted on Approve, so it is drawn faint.
                let name = text(&t.name).size(SIZE_BODY);
                let name = if ticked { name } else { name.style(style::faint) };
                let label = row![name, text(&t.feed_title).size(SIZE_SMALL).style(style::muted)]
                    .spacing(SPACE[1])
                    .align_y(Center);
                let id = t.id;
                let tick =
                    checkbox(ticked).size(SIZE_SMALL).style(style::checkbox).on_toggle(move |_| DraftMsg::TickTerm(id));
                col = col.push(
                    container(
                        row![tick, self.grip(DItem::Term(t.id), &t.name, label)].spacing(SPACE[1]).align_y(Center),
                    )
                    .padding(iced::padding::left(2.0 * INDENT).top(2.0).bottom(2.0)),
                );
            }
        }
        if matches!(kind, Some(DItem::Term(_))) { self.zone(col, DTarget::Category(c)) } else { col.into() }
    }

    fn view_add_group(&self) -> Element<'_, DraftMsg> {
        match &self.edit {
            Some(Edit::NewGroup(n)) => name_field("Group name", n),
            _ => button(text("+ New group").size(SIZE_SMALL).style(style::accent))
                .padding([0.0, SPACE[0]])
                .style(style::row)
                .on_press(DraftMsg::NewGroup)
                .into(),
        }
    }
}

fn yours<'a>() -> iced::widget::Text<'a> {
    text("yours").size(SIZE_SMALL).style(style::success)
}

/// A category's name anywhere in the tree.
fn category_name(snap: &Snapshot, id: CategoryId) -> Option<&str> {
    snap.groups
        .iter()
        .flat_map(|g| &g.categories)
        .chain(&snap.ungrouped)
        .find(|c| c.category.id == id)
        .map(|c| c.category.name.as_str())
}

pub(crate) fn vague_line(n: usize) -> Option<String> {
    match n {
        0 => None,
        1 => Some("1 term was too vague to sort and stays unsorted.".into()),
        n => Some(format!("{n} terms were too vague to sort and stay unsorted.")),
    }
}

fn more_terms_line(n: usize) -> String {
    format!("{n} more {} will be offered next time.", if n == 1 { "term" } else { "terms" })
}

/// The inline name field with its Save button: Enter or Save saves, Esc cancels, and opening
/// another name field saves what is typed here.
fn name_field<'a>(placeholder: &'a str, value: &'a str) -> Element<'a, DraftMsg> {
    let save = button(text("Save").size(SIZE_SMALL).style(style::accent))
        .padding([0.0, SPACE[0]])
        .style(style::row)
        .on_press_maybe((!value.trim().is_empty()).then_some(DraftMsg::SaveName));
    row![name_input(placeholder, value), save].spacing(SPACE[0]).align_y(Center).into()
}

/// The text input itself: Enter saves.
fn name_input<'a>(placeholder: &'a str, value: &'a str) -> iced::widget::TextInput<'a, DraftMsg> {
    text_input(placeholder, value)
        .id(NAME_ID)
        .style(style::text_input)
        .on_input(DraftMsg::EditName)
        .on_submit(DraftMsg::SaveName)
        .size(SIZE_BODY)
        .padding(SPACE[0])
        .width(260)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_draft_says_why_nothing_was_saved() {
        let mut d = CategoryDraft::default();
        assert_eq!(empty_reason(&d), NO_SUGGESTIONS);
        d.grouping_skipped = true;
        assert_eq!(empty_reason(&d), GROUPING_SKIPPED);
    }
}

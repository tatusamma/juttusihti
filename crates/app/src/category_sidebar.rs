//! The categories sidebar of the reader, between the rail and the article list:
//! All articles, Uncategorized, Favourites, the groups with their categories, Ungrouped and
//! Organise…. Clicking a row picks what the list shows; dragging it reorders
//! favourites, groups and categories (the same order as the Organise screen).
//! "Find category…" turns the tree into a flat list of matching names.

use std::collections::HashSet;

use iced::keyboard::{self, key};
use iced::widget::{Space, button, column, container, mouse_area, row, scrollable, stack, text, text_input, tooltip};
use iced::{Center, Element, Fill, Point, Subscription, Task as Cmd, event, mouse};
use juttusihti_core::FeedId;
use juttusihti_core::categories::CategoryId;
use juttusihti_core::categories::{
    CategoryCounts, CategoryNode, CategoryTree, Favourite, GroupFeed, GroupId, GroupNode,
};
use juttusihti_core::normalize::normalize_text;
use juttusihti_core::pricing::Catalog;
use juttusihti_core::store::{ListFilter, Store};

use crate::categorize_ai::{self, Panel, Plan};
use crate::db::Db;
use crate::drag::{self, DragMsg, DragState};
use crate::icons::{Icon, icon};
use crate::style::{self, SEMIBOLD, SIZE_BODY, SIZE_SMALL, SPACE};

/// Setting that remembers whether the sidebar is shown: "1" or "0" (default shown).
pub const OPEN_KEY: &str = "sidebar.categories.open";

/// Width of the sidebar.
pub const WIDTH: f32 = 220.0;

/// Left indent of a category under its group heading.
const INDENT: f32 = 18.0;

/// What the article list is narrowed to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Selection {
    #[default]
    All,
    /// Articles in no category.
    Uncategorized,
    Group(GroupId),
    Category(CategoryId),
    /// A feed listed under its group.
    Feed(FeedId),
}

impl From<Favourite> for Selection {
    fn from(f: Favourite) -> Self {
        match f {
            Favourite::Group(g) => Selection::Group(g),
            Favourite::Category(c) => Selection::Category(c),
        }
    }
}

/// A row that can be clicked, dragged and favourited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// A row in Favourites (dragging it reorders the favourites).
    Favourite(Favourite),
    Group(GroupId),
    Category(CategoryId),
}

impl Item {
    fn favourite(self) -> Favourite {
        match self {
            Item::Favourite(f) => f,
            Item::Group(g) => Favourite::Group(g),
            Item::Category(c) => Favourite::Category(c),
        }
    }

    fn selection(self) -> Selection {
        self.favourite().into()
    }
}

/// Where a dragged row can land.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Before the favourite at this index (the count: the end).
    FavouriteSlot(usize),
    /// As on the Organise screen: in a group (`None`: Ungrouped) before its row at
    /// this index; for a dragged group, `Some(g)` means before group `g` and `None` the end.
    GroupSlot(Option<GroupId>, usize),
}

/// Store work asked for by the sidebar.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    SetOpen(bool),
    Favourite(Favourite, bool),
    MoveFavourite(Favourite, usize),
    MoveGroup(GroupId, usize),
    MoveCategory(CategoryId, Option<GroupId>, usize),
    /// Hide a category's articles from the list, or show them again.
    SetHidden(CategoryId, bool),
}

impl Op {
    pub fn apply(self, s: &mut Store) -> juttusihti_core::Result<()> {
        match self {
            Op::SetOpen(open) => s.set_setting(OPEN_KEY, if open { "1" } else { "0" }),
            Op::Favourite(f, true) => s.add_favourite(f),
            Op::Favourite(f, false) => s.remove_favourite(f),
            Op::MoveFavourite(f, index) => s.move_favourite(f, index),
            Op::MoveGroup(id, index) => s.move_group(id, index),
            Op::MoveCategory(id, group, index) => s.move_category(id, group, index),
            Op::SetHidden(id, hidden) => s.set_category_hidden(id, hidden),
        }
    }
}

/// Whether the sidebar is shown (default: shown).
pub fn read_open(s: &Store) -> juttusihti_core::Result<bool> {
    Ok(s.setting(OPEN_KEY)?.is_none_or(|v| v != "0"))
}

/// What a message asks for next.
#[derive(Debug, PartialEq)]
pub enum Effect {
    None,
    /// Reload the tree and counts after a change made here.
    Reload,
    Run(Op),
    Select(Selection),
    /// Work out the "find categories with AI" plan; the number tells answers apart.
    PlanAi(u64),
    /// Send the consented plan (the running panel's).
    RunAi,
    /// The AI saved categories: reload the tree, the counts and the list.
    Categorized,
    Organise,
    /// New tree and counts arrived.
    Loaded,
    Failed(String),
}

/// What the app has to act on.
#[derive(Debug, PartialEq)]
pub enum Event {
    Select(Selection),
    /// AI saved categories for some articles; the list may differ.
    Categorized,
    Organise,
    /// The tree was reloaded; the selection may be gone.
    Loaded,
    /// Groups or favourites were rearranged here; a group's articles may differ.
    Changed,
    Failed(String),
}

/// The tree, the counts for the list's filter, and the categories with any stored article.
pub(crate) type Data = Box<(CategoryTree, CategoryCounts, HashSet<CategoryId>)>;

/// What the sidebar loads for `filter` (the list's All/Unread and feed picker).
pub(crate) fn read_data(s: &Store, filter: ListFilter) -> juttusihti_core::Result<Data> {
    let counts = s.category_counts(filter)?;
    // Whatever the All/Unread or feed choice, a category with stored articles stays listed.
    let all = if filter == ListFilter::default() { None } else { Some(s.category_counts(ListFilter::default())?) };
    let stored = all.as_ref().unwrap_or(&counts).categories.iter().filter(|(_, n)| **n > 0).map(|(c, _)| *c).collect();
    Ok(Box::new((s.category_tree()?, counts, stored)))
}

#[derive(Debug, Clone)]
pub enum Msg {
    OpenLoaded(Result<bool, String>),
    Loaded(ListFilter, Result<Data, String>),
    /// « and », and the c key.
    Toggle,
    Select(Selection),
    /// The ✨ on the Uncategorized row: opens the "find categories with AI" panel.
    FindWithAi,
    AiPlanned(u64, Result<Box<Plan>, String>),
    SendAi,
    CloseAi,
    AiDone(Result<String, String>),
    Fold(GroupId),
    Find(String),
    ClearFind,
    Escape,
    Hover(Item, bool),
    /// Right-click: opens or closes the row's menu.
    Menu(Item),
    SetFavourite(Favourite, bool),
    /// The eye: hide a category's articles from the list, or show them again.
    SetHidden(CategoryId, bool),
    /// A category was hidden or shown: the list differs.
    HiddenSaved(Result<(), String>),
    Drag(DragMsg<Item, Target>),
    Organise,
    Done(Result<(), String>),
    Saved(Result<(), String>),
}

/// A row of the flat "Find category…" list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match<'a> {
    pub selection: Selection,
    pub name: &'a str,
    /// The group a category is in, shown in grey after it.
    pub group: Option<&'a str>,
}

pub struct State {
    db: Db,
    pub open: bool,
    tree: CategoryTree,
    counts: CategoryCounts,
    /// Categories that hold at least one stored article; the others are not listed.
    stored: HashSet<CategoryId>,
    /// The All/Unread and feed picker filter the counts were asked for.
    filter: ListFilter,
    /// Unfolded groups; kept in memory only.
    expanded: HashSet<GroupId>,
    pub find: String,
    drag: DragState<Item, Target>,
    hover: Option<Item>,
    menu: Option<Item>,
    /// The "find categories with AI" panel, when open.
    ai: Option<Panel>,
    /// Tells the latest plan from older ones.
    ai_generation: u64,
    catalog: Catalog,
}

impl State {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            open: true,
            tree: CategoryTree { groups: vec![], ungrouped: vec![], favourites: vec![], ungrouped_feeds: vec![] },
            counts: CategoryCounts::default(),
            stored: HashSet::new(),
            filter: ListFilter::default(),
            expanded: HashSet::new(),
            find: String::new(),
            drag: DragState::default(),
            hover: None,
            menu: None,
            ai: None,
            ai_generation: 0,
            catalog: Catalog::builtin(),
        }
    }

    /// Reads whether the sidebar is shown; once, at start.
    pub fn load_open(&self) -> Cmd<Msg> {
        let db = self.db.clone();
        Cmd::perform(async move { db.call(|s| read_open(s)).await }, Msg::OpenLoaded)
    }

    /// Reloads the tree and the counts for `filter` (the list's All/Unread and feed picker).
    pub fn load(&mut self, filter: ListFilter) -> Cmd<Msg> {
        // An open AI panel offers the articles of the scope being viewed.
        let replan = if filter != self.filter && matches!(self.ai, Some(Panel::Planning | Panel::Ready(_))) {
            self.ai_generation += 1;
            self.ai = Some(Panel::Planning);
            self.plan_ai(self.ai_generation, filter)
        } else {
            Cmd::none()
        };
        self.filter = filter;
        let db = self.db.clone();
        let load =
            Cmd::perform(async move { db.call(move |s| read_data(s, filter)).await }, move |r| Msg::Loaded(filter, r));
        Cmd::batch([load, replan])
    }

    fn plan_ai(&self, generation: u64, filter: ListFilter) -> Cmd<Msg> {
        Cmd::perform(categorize_ai::plan(self.db.clone(), self.catalog.clone(), filter), move |r| {
            Msg::AiPlanned(generation, r)
        })
    }

    /// The loaded tree of groups and categories.
    pub fn tree(&self) -> &CategoryTree {
        &self.tree
    }

    /// The "find categories with AI" panel, when open.
    #[cfg(test)]
    pub fn ai_panel(&self) -> Option<&Panel> {
        self.ai.as_ref()
    }

    pub fn update(&mut self, msg: Msg) -> (Cmd<Msg>, Option<Event>) {
        match self.handle(msg) {
            Effect::None => (Cmd::none(), None),
            Effect::Reload => (self.load(self.filter), Some(Event::Changed)),
            Effect::Run(op) => {
                let db = self.db.clone();
                let done = match op {
                    Op::SetOpen(_) => Msg::Saved,
                    Op::SetHidden(..) => Msg::HiddenSaved,
                    _ => Msg::Done,
                };
                (Cmd::perform(async move { db.call(move |s| op.apply(s)).await }, done), None)
            }
            Effect::Select(s) => (Cmd::none(), Some(Event::Select(s))),
            Effect::PlanAi(generation) => (self.plan_ai(generation, self.filter), None),
            Effect::RunAi => match &self.ai {
                Some(Panel::Running(p)) => {
                    (Cmd::perform(categorize_ai::run(self.db.clone(), (**p).clone()), Msg::AiDone), None)
                }
                _ => (Cmd::none(), None),
            },
            Effect::Categorized => (self.load(self.filter), Some(Event::Categorized)),
            Effect::Organise => (Cmd::none(), Some(Event::Organise)),
            Effect::Loaded => (Cmd::none(), Some(Event::Loaded)),
            Effect::Failed(e) => (Cmd::none(), Some(Event::Failed(e))),
        }
    }

    pub(crate) fn handle(&mut self, msg: Msg) -> Effect {
        match msg {
            Msg::OpenLoaded(Ok(open)) => self.open = open,
            Msg::Loaded(filter, Ok(data)) => {
                let (tree, counts, stored) = *data;
                self.tree = tree;
                self.stored = stored;
                // Counts for an older All/Unread or feed choice wait for the newer load.
                if filter == self.filter {
                    self.counts = counts;
                }
                let groups: HashSet<GroupId> = self.tree.groups.iter().map(|g| g.group.id).collect();
                self.expanded.retain(|g| groups.contains(g));
                if self.hover.is_some_and(|i| !self.contains(i.selection())) {
                    self.hover = None;
                }
                if self.menu.is_some_and(|i| !self.contains(i.selection())) {
                    self.menu = None;
                }
                return Effect::Loaded;
            }
            Msg::Toggle => {
                self.open = !self.open;
                self.menu = None;
                // A hidden sidebar cannot finish a drag.
                self.cancel_drag();
                return Effect::Run(Op::SetOpen(self.open));
            }
            Msg::Select(s) => {
                self.menu = None;
                // Unfold the group of a picked category so the selection shows in the tree.
                let group = match s {
                    Selection::Category(id) => self.node(id).and_then(|c| c.group_id),
                    Selection::Feed(f) => self.feed_group(f),
                    _ => None,
                };
                if let Some(g) = group {
                    self.expanded.insert(g);
                }
                return Effect::Select(s);
            }
            Msg::Fold(g) => {
                if !self.expanded.remove(&g) {
                    self.expanded.insert(g);
                }
            }
            Msg::Find(s) => {
                self.find = s;
                self.menu = None;
            }
            Msg::ClearFind => self.find.clear(),
            Msg::Escape => {
                self.drag.update(DragMsg::Cancel);
                self.menu = None;
                self.find.clear();
            }
            Msg::Hover(item, true) => self.hover = Some(item),
            Msg::Hover(item, false) => {
                if self.hover == Some(item) {
                    self.hover = None;
                }
            }
            Msg::Menu(item) => self.menu = if self.menu == Some(item) { None } else { Some(item) },
            Msg::SetFavourite(f, on) => {
                self.menu = None;
                return Effect::Run(Op::Favourite(f, on));
            }
            Msg::SetHidden(c, hidden) => {
                self.menu = None;
                return Effect::Run(Op::SetHidden(c, hidden));
            }
            // The list reloads as after AI categorizing.
            Msg::HiddenSaved(Ok(())) => return Effect::Categorized,
            Msg::Drag(m) => {
                // A press and release without movement is a click.
                let click = match m {
                    DragMsg::Release => self.drag.dragging().filter(|d| !d.active).map(|d| d.item),
                    _ => None,
                };
                if let Some((item, target)) = self.drag.update(m) {
                    return self.drop_op(item, target).map_or(Effect::None, Effect::Run);
                }
                if let Some(item) = click {
                    return self.handle(Msg::Select(item.selection()));
                }
            }
            Msg::FindWithAi => {
                self.menu = None;
                if matches!(self.ai, Some(Panel::Planning | Panel::Running(_))) {
                    return Effect::None;
                }
                self.ai_generation += 1;
                self.ai = Some(Panel::Planning);
                return Effect::PlanAi(self.ai_generation);
            }
            Msg::AiPlanned(g, r) => {
                if g == self.ai_generation && matches!(self.ai, Some(Panel::Planning)) {
                    self.ai = Some(match r {
                        Ok(p) => Panel::Ready(p),
                        Err(e) => Panel::Failed(e),
                    });
                }
            }
            Msg::SendAi => {
                if let Some(Panel::Ready(p)) = &self.ai
                    && p.blocker().is_none()
                {
                    self.ai = Some(Panel::Running(p.clone()));
                    return Effect::RunAi;
                }
            }
            Msg::CloseAi => {
                if !matches!(self.ai, Some(Panel::Running(_))) {
                    self.ai = None;
                }
            }
            Msg::AiDone(Ok(line)) => {
                self.ai = Some(Panel::Done(line));
                return Effect::Categorized;
            }
            Msg::AiDone(Err(e)) => self.ai = Some(Panel::Failed(e)),
            Msg::Organise => return Effect::Organise,
            Msg::Done(Ok(())) => return Effect::Reload,
            Msg::Saved(Ok(())) => {}
            Msg::OpenLoaded(Err(e))
            | Msg::Loaded(_, Err(e))
            | Msg::Done(Err(e))
            | Msg::Saved(Err(e))
            | Msg::HiddenSaved(Err(e)) => {
                return Effect::Failed(e);
            }
        }
        Effect::None
    }

    /// Pointer tracking while a row is pressed, and Esc (also from inside the
    /// find field) while something is open.
    pub fn subscription(&self) -> Subscription<Msg> {
        let dragging = self.drag.dragging().is_some();
        let pointer = if dragging {
            drag::subscription(|p| Msg::Drag(DragMsg::Move(p)), || Msg::Drag(DragMsg::Release))
        } else {
            Subscription::none()
        };
        let escape = if dragging || self.menu.is_some() || !self.find.is_empty() {
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

    /// Drops a pressed or dragged row, e.g. when the reader is left mid-drag.
    pub fn cancel_drag(&mut self) {
        self.drag.update(DragMsg::Cancel);
    }

    /// Whether a row is pressed or being dragged (the pointer is tracked).
    #[cfg(test)]
    pub(crate) fn armed(&self) -> bool {
        self.drag.dragging().is_some()
    }

    /// The label and cursor position of the chip to draw at the pointer, while dragging.
    pub fn ghost(&self) -> Option<(&str, Point)> {
        self.drag.dragging().filter(|d| d.active).map(|d| (d.label.as_str(), d.cursor))
    }

    /// The row being dragged, once the pointer has moved past the drag threshold.
    /// Every click presses a row, so the drop zones must not appear on a mere press
    /// (they would make the tree jump on each selection).
    pub(crate) fn dragged(&self) -> Option<Item> {
        self.drag.dragging().filter(|d| d.active).map(|d| d.item)
    }

    fn group(&self, id: GroupId) -> Option<&GroupNode> {
        self.tree.groups.iter().find(|g| g.group.id == id)
    }

    fn node(&self, id: CategoryId) -> Option<&CategoryNode> {
        self.tree.groups.iter().flat_map(|g| &g.categories).chain(&self.tree.ungrouped).find(|c| c.category.id == id)
    }

    /// The group a feed is listed in.
    fn feed_group(&self, feed: FeedId) -> Option<GroupId> {
        self.tree.groups.iter().find(|g| g.feeds.iter().any(|f| f.id == feed)).map(|g| g.group.id)
    }

    fn group_feed(&self, feed: FeedId) -> Option<&GroupFeed> {
        self.tree.groups.iter().flat_map(|g| &g.feeds).find(|f| f.id == feed)
    }

    /// Whether the selection still exists.
    pub fn contains(&self, s: Selection) -> bool {
        match s {
            Selection::All | Selection::Uncategorized => true,
            Selection::Group(g) => self.group(g).is_some(),
            Selection::Category(c) => self.node(c).is_some(),
            Selection::Feed(f) => self.group_feed(f).is_some(),
        }
    }

    /// The selection's name; `None` for All or a selection that is gone.
    pub fn name(&self, s: Selection) -> Option<&str> {
        match s {
            Selection::All => None,
            Selection::Uncategorized => Some("Uncategorized"),
            Selection::Group(g) => self.group(g).map(|g| g.group.name.as_str()),
            Selection::Category(c) => self.node(c).map(|c| c.category.name.as_str()),
            Selection::Feed(f) => self.group_feed(f).map(|f| f.title.as_str()),
        }
    }

    /// Whether a category's articles are left out of the list.
    pub fn is_hidden(&self, c: CategoryId) -> bool {
        self.node(c).is_some_and(|n| n.hidden)
    }

    /// Whether a category is listed: it holds stored articles, is selected, or a row is
    /// being dragged (so the drop places stay put).
    fn listed(&self, c: CategoryId, selected: Selection) -> bool {
        self.stored.contains(&c) || selected == Selection::Category(c) || self.dragged().is_some()
    }

    /// Whether a group is listed: it has feeds or listed categories, or is selected.
    fn group_listed(&self, g: &GroupNode, selected: Selection) -> bool {
        !g.feeds.is_empty()
            || selected == Selection::Group(g.group.id)
            || self.dragged().is_some()
            || g.categories.iter().any(|c| self.listed(c.category.id, selected))
    }

    pub fn is_favourite(&self, f: Favourite) -> bool {
        self.tree.favourites.contains(&f)
    }

    /// The right-click menu entry for `f`.
    pub fn menu_label(&self, f: Favourite) -> &'static str {
        if self.is_favourite(f) { "Remove from favourites" } else { "Add to favourites" }
    }

    fn count(&self, s: Selection) -> Option<u64> {
        match s {
            Selection::All => None,
            Selection::Uncategorized => Some(self.counts.uncategorized),
            Selection::Group(g) => Some(self.counts.groups.get(&g).copied().unwrap_or(0)),
            Selection::Category(c) => Some(self.counts.categories.get(&c).copied().unwrap_or(0)),
            Selection::Feed(f) => Some(self.counts.feeds.get(&f).copied().unwrap_or(0)),
        }
    }

    /// Groups, categories and group feeds whose names contain the "Find category…" text
    /// (ignoring case and punctuation), in tree order.
    pub fn matches(&self) -> Vec<Match<'_>> {
        let q = normalize_text(&self.find);
        if q.is_empty() {
            return vec![];
        }
        let hit = |name: &str| normalize_text(name).contains(&q);
        let mut out = vec![];
        for g in &self.tree.groups {
            if hit(&g.group.name) {
                out.push(Match { selection: Selection::Group(g.group.id), name: &g.group.name, group: None });
            }
            for c in g.categories.iter().filter(|c| self.stored.contains(&c.category.id) && hit(&c.category.name)) {
                out.push(Match {
                    selection: Selection::Category(c.category.id),
                    name: &c.category.name,
                    group: Some(&g.group.name),
                });
            }
            for f in g.feeds.iter().filter(|f| hit(&f.title)) {
                out.push(Match { selection: Selection::Feed(f.id), name: &f.title, group: Some(&g.group.name) });
            }
        }
        for c in self.tree.ungrouped.iter().filter(|c| self.stored.contains(&c.category.id) && hit(&c.category.name)) {
            out.push(Match { selection: Selection::Category(c.category.id), name: &c.category.name, group: None });
        }
        out
    }

    /// The categories of a group, or the ungrouped ones.
    fn members(&self, group: Option<GroupId>) -> &[CategoryNode] {
        match group {
            Some(g) => self.group(g).map_or(&[], |g| &g.categories),
            None => &self.tree.ungrouped,
        }
    }

    /// The store work for dropping `item` on `target`; `None` when nothing would change.
    fn drop_op(&self, item: Item, target: Target) -> Option<Op> {
        match (item, target) {
            (Item::Favourite(f), Target::FavouriteSlot(before)) => {
                let from = self.tree.favourites.iter().position(|x| *x == f)?;
                drag::reorder(from, before).map(|to| Op::MoveFavourite(f, to))
            }
            (Item::Group(id), Target::GroupSlot(before, _)) => {
                let index = |g: GroupId| self.tree.groups.iter().position(|x| x.group.id == g);
                let from = index(id)?;
                let before = match before {
                    Some(b) => index(b)?,
                    None => self.tree.groups.len(),
                };
                drag::reorder(from, before).map(|to| Op::MoveGroup(id, to))
            }
            (Item::Category(id), Target::GroupSlot(group, index)) => {
                self.node(id)?;
                match self.members(group).iter().position(|c| c.category.id == id) {
                    Some(from) => drag::reorder(from, index).map(|to| Op::MoveCategory(id, group, to)),
                    None => Some(Op::MoveCategory(id, group, index)),
                }
            }
            _ => None,
        }
    }

    // ---- view ----------------------------------------------------------

    pub fn view(&self, selected: Selection) -> Element<'_, Msg> {
        let header = row![
            text("Categories").size(SIZE_SMALL).style(style::muted).width(Fill),
            button(text("Organise…").size(SIZE_SMALL).style(style::accent))
                .padding([0.0, SPACE[1]])
                .style(style::row)
                .on_press(Msg::Organise),
            tooltip(
                button(text("«").size(SIZE_BODY)).padding([0.0, SPACE[1]]).style(style::icon).on_press(Msg::Toggle),
                style::tip("Hide categories (c)"),
                tooltip::Position::Bottom,
            ),
        ]
        .align_y(Center);

        let mut find = row![
            text_input("Find category…", &self.find)
                .on_input(Msg::Find)
                .style(style::text_input)
                .size(SIZE_SMALL)
                .padding([SPACE[0], SPACE[1]])
                .width(Fill)
        ]
        .spacing(SPACE[0])
        .align_y(Center);
        if !self.find.is_empty() {
            find = find.push(style::icon_button(Icon::Close, "Clear (Esc)", SIZE_SMALL, Some(Msg::ClearFind)));
        }

        let body = if self.find.is_empty() { self.view_tree(selected) } else { self.view_matches(selected) };
        let mut col = column![container(header).padding([0.0, SPACE[1]])].spacing(SPACE[1]);
        if let Some(panel) = &self.ai {
            col = col.push(categorize_ai::view(panel));
        }
        container(col.push(find).push(scrollable(container(body).padding(iced::padding::right(SPACE[1]))).height(Fill)))
            .padding([SPACE[2], SPACE[1]])
            .width(WIDTH)
            .height(Fill)
            .style(style::pane)
            .into()
    }

    fn view_matches(&self, selected: Selection) -> Element<'_, Msg> {
        let found = self.matches();
        if found.is_empty() {
            return container(style::note("Nothing matches.")).padding([SPACE[1], SPACE[1]]).into();
        }
        let mut col = column![].spacing(1);
        for m in found {
            let mut label = row![text(m.name).size(SIZE_BODY)].spacing(SPACE[0]).align_y(Center);
            if let Some(g) = m.group {
                label = label.push(text(format!("· {g}")).size(SIZE_SMALL).style(style::faint));
            }
            col = col.push(plain_row(label.into(), self.count(m.selection), m.selection == selected, m.selection));
        }
        col.into()
    }

    fn view_tree(&self, selected: Selection) -> Element<'_, Msg> {
        let dragged = self.dragged();
        let mut col = column![
            plain_row(text("All articles").size(SIZE_BODY).into(), None, selected == Selection::All, Selection::All),
            row![
                plain_row(
                    text("Uncategorized").size(SIZE_BODY).into(),
                    self.count(Selection::Uncategorized),
                    selected == Selection::Uncategorized,
                    Selection::Uncategorized
                ),
                style::icon_button(Icon::Sparkles, "Find categories with AI", SIZE_SMALL, Some(Msg::FindWithAi)),
            ]
            .align_y(Center),
        ]
        .spacing(1);

        if !self.tree.favourites.is_empty() {
            let heading = row![
                icon(Icon::Star).size(SIZE_SMALL).style(style::accent),
                text("Favourites").size(SIZE_SMALL).style(style::muted)
            ]
            .spacing(SPACE[0])
            .align_y(Center);
            let mut favs = column![container(heading).padding([SPACE[1], SPACE[1]])].spacing(1);
            for (i, f) in self.tree.favourites.iter().enumerate() {
                let Some(name) = self.name((*f).into()) else { continue };
                if let Favourite::Category(c) = f
                    && !self.listed(*c, selected)
                {
                    continue;
                }
                let r = self.item_row(Item::Favourite(*f), name, selected, 0.0);
                favs = favs.push(match dragged {
                    Some(Item::Favourite(_)) => self.slot(r, Target::FavouriteSlot(i)),
                    _ => r,
                });
            }
            let favs: Element<'_, Msg> = match dragged {
                Some(Item::Favourite(_)) => self.zone(favs, Target::FavouriteSlot(self.tree.favourites.len())),
                _ => favs.into(),
            };
            col = col.push(favs);
        }

        col = col.push(Space::new().height(SPACE[1]));
        for g in self.tree.groups.iter().filter(|g| self.group_listed(g, selected)) {
            col = col.push(self.view_group(g, selected, dragged));
        }
        let moving_rows = matches!(dragged, Some(Item::Group(_) | Item::Category(_)));
        let any_ungrouped = self.tree.ungrouped.iter().any(|c| self.listed(c.category.id, selected));
        if any_ungrouped || moving_rows {
            let cats = &self.tree.ungrouped;
            let block = column![
                container(text("Ungrouped").size(SIZE_SMALL).style(style::muted)).padding([SPACE[1], SPACE[1]]),
                self.view_members(None, cats, selected, dragged, 0.0),
            ]
            .spacing(1);
            col = col.push(match dragged {
                // Above Ungrouped is after the last group.
                Some(Item::Group(_)) => self.slot(block, Target::GroupSlot(None, 0)),
                Some(Item::Category(_)) => self.zone(block, Target::GroupSlot(None, cats.len())),
                _ => block.into(),
            });
        }
        if !any_ungrouped && !self.tree.groups.iter().any(|g| self.group_listed(g, selected)) {
            col = col.push(container(style::note("No categories yet.")).padding([SPACE[0], SPACE[1]]));
        }
        col.into()
    }

    fn view_group<'a>(&'a self, g: &'a GroupNode, selected: Selection, dragged: Option<Item>) -> Element<'a, Msg> {
        let id = g.group.id;
        let open = self.expanded.contains(&id);
        let mut block = column![self.item_row(Item::Group(id), &g.group.name, selected, 0.0)].spacing(1);
        if open {
            block = block.push(self.view_members(Some(id), &g.categories, selected, dragged, INDENT));
            for f in &g.feeds {
                block = block.push(feed_row(f, self.count(Selection::Feed(f.id)), selected == Selection::Feed(f.id)));
            }
        }
        match dragged {
            Some(Item::Group(_)) => self.slot(block, Target::GroupSlot(Some(id), 0)),
            Some(Item::Category(_)) => self.zone(block, Target::GroupSlot(Some(id), g.categories.len())),
            _ => block.into(),
        }
    }

    fn view_members<'a>(
        &'a self,
        group: Option<GroupId>,
        cats: &'a [CategoryNode],
        selected: Selection,
        dragged: Option<Item>,
        indent: f32,
    ) -> Element<'a, Msg> {
        let mut col = column![].spacing(1);
        for (i, c) in cats.iter().enumerate() {
            if !self.listed(c.category.id, selected) {
                continue;
            }
            let r = self.item_row(Item::Category(c.category.id), &c.category.name, selected, indent);
            col = col.push(match dragged {
                Some(Item::Category(_)) => self.slot(r, Target::GroupSlot(group, i)),
                _ => r,
            });
        }
        col.into()
    }

    fn over(&self, t: Target) -> bool {
        self.drag.dragging().is_some_and(|d| d.active && d.over == Some(t))
    }

    fn zone<'a>(&self, content: impl Into<Element<'a, Msg>>, t: Target) -> Element<'a, Msg> {
        drag::target(content.into(), self.over(t), Msg::Drag(DragMsg::Enter(t)), Msg::Drag(DragMsg::Leave(t)))
    }

    fn slot<'a>(&self, content: impl Into<Element<'a, Msg>>, t: Target) -> Element<'a, Msg> {
        drag::insertion(content.into(), self.over(t), Msg::Drag(DragMsg::Enter(t)), Msg::Drag(DragMsg::Leave(t)))
    }

    /// A favourite, group or category row: click selects, drag reorders,
    /// right-click opens its menu, hover shows the favourite star.
    fn item_row<'a>(&'a self, item: Item, name: &'a str, selected: Selection, indent: f32) -> Element<'a, Msg> {
        let fav = item.favourite();
        let is_fav = self.is_favourite(fav);
        let hovered = self.hover == Some(item);
        let mut content = row![].spacing(SPACE[0]).align_y(Center);
        if indent > 0.0 {
            content = content.push(Space::new().width(indent));
        }
        match item {
            Item::Group(g) => {
                let open = self.expanded.contains(&g);
                content = content.push(
                    button(icon(if open { Icon::ChevronDown } else { Icon::ChevronRight }).size(SIZE_SMALL))
                        .padding(0)
                        .style(style::icon)
                        .on_press(Msg::Fold(g)),
                );
            }
            Item::Favourite(_) => content = content.push(icon(Icon::Star).size(SIZE_SMALL).style(style::accent)),
            Item::Category(_) => {}
        }
        let category = match item {
            Item::Category(c) | Item::Favourite(Favourite::Category(c)) => Some(c),
            _ => None,
        };
        let hidden = category.is_some_and(|c| self.is_hidden(c));
        let label = text(name).size(SIZE_BODY).width(Fill);
        let label = if hidden { label.style(style::faint) } else { label };
        let strong = matches!(item, Item::Group(_) | Item::Favourite(Favourite::Group(_)));
        content = content.push(if strong { label.font(SEMIBOLD) } else { label });
        if hovered || self.menu == Some(item) {
            let star = icon(Icon::Star).size(SIZE_SMALL).style(if is_fav { style::accent } else { style::faint });
            content = content.push(tooltip(
                button(star).padding(0).style(style::icon).on_press(Msg::SetFavourite(fav, !is_fav)),
                style::tip(self.menu_label(fav)),
                tooltip::Position::Top,
            ));
        }
        // A hidden category always shows its crossed eye, so it is easy to show again.
        if let Some(c) = category
            && (hidden || hovered || self.menu == Some(item))
        {
            let eye = icon(if hidden { Icon::EyeOff } else { Icon::Eye }).size(SIZE_SMALL).style(if hidden {
                style::accent
            } else {
                style::faint
            });
            content = content.push(tooltip(
                button(eye).padding(0).style(style::icon).on_press(Msg::SetHidden(c, !hidden)),
                style::tip(hide_label(hidden)),
                tooltip::Position::Top,
            ));
        }
        if let Some(n) = self.count(item.selection()) {
            content = content.push(text(n.to_string()).size(SIZE_SMALL).style(style::faint));
        }
        let is_selected = item.selection() == selected;
        let surface = container(content)
            .padding([SPACE[0], SPACE[1]])
            .width(Fill)
            .style(style::row_surface(hovered || is_selected));
        let surface: Element<'_, Msg> = if is_selected { with_edge(surface.into()) } else { surface.into() };
        let label = name.to_string();
        // The release is caught here too: the window-wide release subscription starts only
        // after the press, so a tap whose press and release arrive together would lose it.
        let r = mouse_area(surface)
            .on_press(Msg::Drag(DragMsg::Press(item, label, Point::new(f32::NAN, f32::NAN))))
            .on_release(Msg::Drag(DragMsg::Release))
            .on_right_press(Msg::Menu(item))
            .on_enter(Msg::Hover(item, true))
            .on_exit(Msg::Hover(item, false))
            .interaction(mouse::Interaction::Pointer);
        if self.menu != Some(item) {
            return r.into();
        }
        let entry = button(text(self.menu_label(fav)).size(SIZE_SMALL))
            .width(Fill)
            .padding([SPACE[0], SPACE[1]])
            .style(style::row)
            .on_press(Msg::SetFavourite(fav, !is_fav));
        let mut entries = column![entry].spacing(1);
        if let Some(c) = category {
            entries = entries.push(
                button(text(hide_label(hidden)).size(SIZE_SMALL))
                    .width(Fill)
                    .padding([SPACE[0], SPACE[1]])
                    .style(style::row)
                    .on_press(Msg::SetHidden(c, !hidden)),
            );
        }
        let menu = container(entries).padding(SPACE[0]).width(Fill).style(style::popover);
        column![r, container(menu).padding(iced::padding::left(indent + SPACE[1]))].spacing(2).into()
    }
}

/// The eye's tooltip and menu entry.
fn hide_label(hidden: bool) -> &'static str {
    if hidden { "Show in feed" } else { "Hide from feed" }
}

/// A row that only selects (All articles and Find results).
fn plain_row(label: Element<'_, Msg>, count: Option<u64>, selected: bool, s: Selection) -> Element<'_, Msg> {
    let mut content = row![container(label).width(Fill)].spacing(SPACE[0]).align_y(Center);
    if let Some(n) = count {
        content = content.push(text(n.to_string()).size(SIZE_SMALL).style(style::faint));
    }
    let b = button(content)
        .width(Fill)
        .padding([SPACE[0], SPACE[1]])
        .style(if selected { style::row_selected } else { style::row })
        .on_press(Msg::Select(s));
    if selected { with_edge(b.into()) } else { b.into() }
}

/// A feed under its group: clicking selects; it cannot be dragged, dropped on or favourited.
fn feed_row(f: &GroupFeed, count: Option<u64>, selected: bool) -> Element<'_, Msg> {
    let label = row![
        Space::new().width(INDENT),
        icon(Icon::Rss).size(SIZE_SMALL).style(style::faint),
        text(&f.title).size(SIZE_BODY)
    ]
    .spacing(SPACE[0])
    .align_y(Center);
    plain_row(label.into(), count, selected, Selection::Feed(f.id))
}

/// The thin accent edge of the selected row, as in the article list.
fn with_edge(row: Element<'_, Msg>) -> Element<'_, Msg> {
    let edge = container(Space::new().width(2).height(Fill)).style(style::accent_edge);
    stack![row, edge].width(Fill).into()
}

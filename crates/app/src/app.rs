//! Application state, messages, update and view.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iced::keyboard::{self, Key};
use iced::widget::{
    Space, button, column, container, operation, responsive, rich_text, row, rule, scrollable, span, stack, text,
    text_input, tooltip,
};
use iced::{Center, Element, Fill, Font, Padding, Point, Size, Subscription, Task, Theme, font};
use juttusihti_analytics::{AnalyticsWorker, Limits};
use juttusihti_core::search::SearchQuery;
use juttusihti_core::store::ListFilter;
use juttusihti_core::{
    Article, ArticleId, ArticleLabel, ArticleSummary, Feed, FeedId, Folder, FolderId, NewFeed, Page, View,
};
use juttusihti_fetch::{Fetcher, Policy};
use tokio::sync::Semaphore;

use crate::db::Db;
use crate::icons::{Icon, icon};
use crate::style::{self, READ_WIDTH, SIZE_ARTICLE_TITLE, SIZE_BODY, SIZE_SMALL, SIZE_TITLE, SPACE};
use crate::{feeds_menu, format, rail};

const PAGE_SIZE: u32 = 50;
const MAX_PARALLEL_FETCHES: usize = 4;
const SEARCH_ID: &str = "search";
/// Fixed heights, so the feeds dropdown can be anchored under its button.
const TOP_BAR_HEIGHT: f32 = 52.0;
const LIST_HEADER_HEIGHT: f32 = 44.0;
/// Left padding of the list header.
const LIST_HEADER_PAD: f32 = 12.0;
/// Width of the ⋯ menu above the article list.
const LIST_MENU_WIDTH: f32 = 280.0;
/// Diameter of the unread dot in the article list.
const UNREAD_DOT: f32 = 6.0;
const SEMIBOLD: Font = Font { weight: font::Weight::Semibold, ..Font::DEFAULT };

/// Which list the reader shows: the news stream or the saved articles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    News,
    Saved,
}

/// Which feeds News shows. Saved has no scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    All,
    Folder(FolderId),
    Feed(FeedId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Reader,
    Reports,
    Research,
    Labels,
    Review,
    Graph,
    Timeline,
    Map,
    Providers,
    Settings,
}

#[derive(Debug, Clone)]
pub struct Sidebar {
    pub(crate) feeds: Vec<Feed>,
    pub(crate) folders: Vec<Folder>,
    pub(crate) unread: Vec<(FeedId, u64)>,
}

#[derive(Debug, Clone)]
pub struct Listing {
    items: Vec<ArticleSummary>,
    total: u64,
    total_is_exact: bool,
    ranked_window: bool,
    interpreted: Option<String>,
    /// AI-rewritten headlines for this page (not rejected ones).
    headlines: std::collections::HashMap<ArticleId, String>,
}

/// Feed being edited in the feeds dropdown.
#[derive(Debug, Clone)]
pub struct FeedEdit {
    pub(crate) id: FeedId,
    pub(crate) title: String,
    pub(crate) folder: Option<FolderId>,
    pub(crate) new_folder: String,
}

/// Pick-list entry for a feed's folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderChoice(pub(crate) Option<FolderId>, pub(crate) String);

impl std::fmt::Display for FolderChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}

#[derive(Debug, Clone)]
pub struct Preview {
    article: Article,
    feed_title: String,
    body: String,
    labels: Vec<ArticleLabel>,
    headline: Option<juttusihti_core::headlines::Headline>,
}

#[derive(Debug, Clone)]
pub enum Message {
    SidebarLoaded(Result<Sidebar, String>),
    GoPlace(Place),
    SetScope(Scope),
    /// Picks All (false) or Unread (true) in News; already shown does nothing.
    SetUnreadOnly(bool),
    ListLoaded(u64, Result<Listing, String>),
    Open(ArticleId),
    PreviewLoaded(Result<Box<Preview>, String>),
    ToggleSaved,
    ToggleRead,
    OpenInBrowser,
    StateChanged(Result<(), String>),
    SearchInput(String),
    SearchSubmit,
    ClearSearch,
    FocusSearch,
    PrevPage,
    NextPage,
    AddFeedInput(String),
    AddFeed,
    FeedAdded(Result<(FeedId, String), String>),
    /// Asks "Unsubscribe from …?"; `ConfirmUnsubscribe` removes the feed.
    RemoveFeed(FeedId),
    ConfirmUnsubscribe,
    CancelUnsubscribe,
    ToggleFeedsMenu,
    /// Opens or closes the ⋯ menu above the article list.
    ToggleListMenu,
    /// Closes any open dropdown (click outside, Esc).
    CloseOverlays,
    MarkAllRead,
    RefreshAll,
    Refreshed(FeedId, Result<usize, String>),
    AutoRefresh,
    ImportOpml,
    Imported(Result<(usize, usize), String>),
    ExportOpml,
    Exported(Result<Option<PathBuf>, String>),
    // No theme switch in the UI until it moves into Settings.
    #[allow(dead_code)]
    ToggleTheme,
    ShowScreen(Screen),
    Research(crate::research::Msg),
    Labels(crate::labels::Msg),
    Review(crate::review::Msg),
    Graph(crate::graph_view::Msg),
    Timeline(crate::timeline_view::Msg),
    Map(crate::map_view::Msg),
    MapArticle,
    ToggleHideDismissed,
    ToggleAiHeadlines,
    DecideHeadline(juttusihti_core::Status),
    HeadlineDecided(Result<(), String>),
    Providers(crate::providers::Msg),
    Quick(crate::quick::Msg),
    /// Opens the quick AI panel for the list being viewed.
    OpenQuick,
    Settings(crate::settings::Msg),
    RetentionApplied(Result<usize, String>),
    LabelInput(String),
    AddLabel,
    RemoveLabel(juttusihti_core::LabelId),
    DecideLabel(juttusihti_core::LabelId, juttusihti_core::Status),
    LabelsChanged(Result<Vec<ArticleLabel>, String>),
    EditFeed(FeedId),
    FeedTitle(String),
    FeedFolder(FolderChoice),
    NewFolder(String),
    SaveFeed,
    CancelEditFeed,
    FeedSaved(Result<(), String>),
    Reports(crate::reports::Msg),
    Key(keyboard::Key, keyboard::Modifiers),
    DismissStatus,
}

pub struct App {
    db: Db,
    fetcher: Option<Fetcher>,
    fetch_slots: Arc<Semaphore>,
    sidebar: Sidebar,
    place: Place,
    scope: Scope,
    unread_only: bool,
    screen: Screen,
    search: String,
    active_search: String,
    page: Page,
    /// Incremented for every list request, so stale responses are dropped.
    list_generation: u64,
    listing: Listing,
    preview: Option<Preview>,
    add_feed: String,
    refreshing: usize,
    status: Option<String>,
    label_input: String,
    feed_edit: Option<FeedEdit>,
    feeds_open: bool,
    /// The ⋯ menu above the article list.
    list_menu_open: bool,
    /// Feed waiting for the "Unsubscribe from …?" answer.
    confirm_unsubscribe: Option<FeedId>,
    reports: crate::reports::State,
    research: crate::research::State,
    labels: crate::labels::State,
    review: crate::review::State,
    graph: crate::graph_view::State,
    timeline: crate::timeline_view::State,
    map: crate::map_view::State,
    hide_dismissed: bool,
    /// Show AI-rewritten headlines in place of clickbait titles.
    ai_headlines: bool,
    providers: crate::providers::State,
    quick: crate::quick::State,
    settings: crate::settings::State,
}

impl App {
    pub fn boot(db: Db, db_path: PathBuf) -> (Self, Task<Message>) {
        let (fetcher, status) = match Fetcher::new(Policy::default()) {
            Ok(f) => (Some(f), None),
            Err(e) => (None, Some(format!("Networking unavailable: {e}"))),
        };
        let analytics = AnalyticsWorker::spawn(db_path.clone(), Limits::default()).ok().map(Arc::new);
        let mut app = Self {
            research: crate::research::State::new(db.clone()),
            labels: crate::labels::State::new(db.clone()),
            review: crate::review::State::new(db.clone()),
            graph: crate::graph_view::State::new(db.clone()),
            timeline: crate::timeline_view::State::new(db.clone()),
            map: crate::map_view::State::new(
                db.clone(),
                db_path.parent().map(|p| p.join("tiles")).unwrap_or_else(|| PathBuf::from("tiles")),
            ),
            hide_dismissed: false,
            ai_headlines: true,
            providers: crate::providers::State::new(db.clone()),
            quick: crate::quick::State::new(db.clone()),
            settings: crate::settings::State::new(db.clone(), db_path.clone()),
            db,
            fetcher,
            fetch_slots: Arc::new(Semaphore::new(MAX_PARALLEL_FETCHES)),
            sidebar: Sidebar { feeds: vec![], folders: vec![], unread: vec![] },
            place: Place::News,
            scope: Scope::All,
            unread_only: false,
            screen: Screen::Reader,
            search: String::new(),
            active_search: String::new(),
            page: Page::first(PAGE_SIZE),
            list_generation: 0,
            listing: Listing {
                items: vec![],
                total: 0,
                total_is_exact: true,
                ranked_window: false,
                interpreted: None,
                headlines: Default::default(),
            },
            preview: None,
            add_feed: String::new(),
            refreshing: 0,
            status,
            label_input: String::new(),
            feed_edit: None,
            feeds_open: false,
            list_menu_open: false,
            confirm_unsubscribe: None,
            reports: crate::reports::State::new(analytics.clone()),
        };
        let task = Task::batch([app.load_sidebar(), app.load_list(), app.settings.load().map(Message::Settings)]);
        (app, task)
    }

    pub fn title(&self) -> String {
        match self.refreshing {
            0 => "Juttusihti".into(),
            n => format!("Juttusihti — refreshing {n}"),
        }
    }

    pub fn theme(&self) -> Theme {
        crate::style::theme(self.settings.values.dark)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let keys = keyboard::listen().filter_map(|event| match event {
            keyboard::Event::KeyPressed { key, modifiers, .. } => Some(Message::Key(key, modifiers)),
            _ => None,
        });
        let minutes = self.settings.values.refresh_minutes;
        let refresh = if minutes == 0 {
            Subscription::none()
        } else {
            iced::time::every(Duration::from_secs(u64::from(minutes) * 60)).map(|_| Message::AutoRefresh)
        };
        Subscription::batch([keys, refresh])
    }

    fn load_sidebar(&self) -> Task<Message> {
        let db = self.db.clone();
        Task::perform(
            async move {
                db.call(|s| -> juttusihti_core::Result<_> {
                    Ok(Sidebar { feeds: s.feeds()?, folders: s.folders()?, unread: s.unread_counts()? })
                })
                .await
            },
            Message::SidebarLoaded,
        )
    }

    /// What the article list shows: Saved is one fixed list, News combines
    /// the remembered scope with the Unread choice.
    fn list_filter(&self) -> ListFilter {
        match self.place {
            Place::Saved => ListFilter { view: View::Saved, ..Default::default() },
            Place::News => {
                let view = if self.unread_only { View::Unread } else { View::All };
                match self.scope {
                    Scope::All => ListFilter { view, ..Default::default() },
                    Scope::Feed(id) => ListFilter { view, feed_id: Some(id), ..Default::default() },
                    Scope::Folder(id) => ListFilter { view, folder_id: Some(id), ..Default::default() },
                }
            }
        }
    }

    /// "All feeds", a folder or feed name, or "Saved".
    fn scope_label(&self) -> String {
        if self.place == Place::Saved {
            return "Saved".into();
        }
        match self.scope {
            Scope::All => "All feeds".into(),
            Scope::Feed(id) => {
                self.sidebar.feeds.iter().find(|f| f.id == id).map_or("Feed".into(), |f| f.title.clone())
            }
            Scope::Folder(id) => {
                self.sidebar.folders.iter().find(|f| f.id == id).map_or("Folder".into(), |f| f.name.clone())
            }
        }
    }

    /// The query behind the article list being viewed.
    fn list_query(&self) -> SearchQuery {
        let filter = self.list_filter();
        SearchQuery {
            text: self.active_search.clone(),
            view: filter.view,
            feed_ids: filter.feed_id.into_iter().collect(),
            folder_ids: filter.folder_id.into_iter().collect(),
            hide_dismissed: self.hide_dismissed,
            ..Default::default()
        }
    }

    fn load_list(&mut self) -> Task<Message> {
        self.list_generation += 1;
        let generation = self.list_generation;
        let db = self.db.clone();
        let page = self.page;
        let query = self.list_query();
        // An open quick panel follows the list being viewed.
        let retarget = if self.quick.open && !self.quick.is_running() && !self.quick.targets(&query) {
            self.quick.open(query.clone(), self.scope_label()).map(Message::Quick)
        } else {
            Task::none()
        };
        let load = Task::perform(
            async move {
                db.call(move |s| -> juttusihti_core::Result<Listing> {
                    let r = s.search(&query, page)?;
                    let ids: Vec<ArticleId> = r.items.iter().map(|a| a.id).collect();
                    Ok(Listing {
                        headlines: s.shown_headlines(&ids)?,
                        items: r.items,
                        total: r.total,
                        total_is_exact: r.total_is_exact,
                        ranked_window: r.ranked_window,
                        interpreted: r.interpreted,
                    })
                })
                .await
            },
            move |r| Message::ListLoaded(generation, r),
        );
        Task::batch([load, retarget])
    }

    fn set_state(&self, id: ArticleId, read: Option<bool>, saved: Option<bool>) -> Task<Message> {
        let db = self.db.clone();
        Task::perform(
            async move {
                db.call(move |s| -> juttusihti_core::Result<()> {
                    if let Some(r) = read {
                        s.set_read(id, r)?;
                    }
                    if let Some(v) = saved {
                        s.set_saved(id, v)?;
                    }
                    Ok(())
                })
                .await
            },
            Message::StateChanged,
        )
    }

    fn refresh_feed(&self, feed: &Feed) -> Option<Task<Message>> {
        let fetcher = self.fetcher.clone()?;
        let db = self.db.clone();
        let slots = self.fetch_slots.clone();
        let (id, url, etag, last_modified) = (feed.id, feed.url.clone(), feed.etag.clone(), feed.last_modified.clone());
        Some(Task::perform(
            async move {
                let _permit = slots.acquire_owned().await.map_err(|e| e.to_string())?;
                let outcome = fetcher.fetch(&url, etag.as_deref(), last_modified.as_deref()).await;
                db.call(move |s| juttusihti_fetch::apply(s, id, outcome).map(|st| st.inserted)).await
            },
            move |r| Message::Refreshed(id, r),
        ))
    }

    fn refresh_all(&mut self) -> Task<Message> {
        let tasks: Vec<_> = self.sidebar.feeds.iter().filter_map(|f| self.refresh_feed(f)).collect();
        self.refreshing += tasks.len();
        Task::batch(tasks)
    }

    fn selected_index(&self) -> Option<usize> {
        let id = self.preview.as_ref()?.article.id;
        self.listing.items.iter().position(|a| a.id == id)
    }

    fn open_relative(&mut self, delta: isize) -> Task<Message> {
        let len = self.listing.items.len();
        if len == 0 {
            return Task::none();
        }
        let next = match self.selected_index() {
            Some(i) => (i as isize + delta).clamp(0, len as isize - 1) as usize,
            None => 0,
        };
        let id = self.listing.items[next].id;
        self.update(Message::Open(id))
    }

    /// Closes the feeds dropdown, dropping an unanswered unsubscribe question and an unsaved edit.
    fn close_feeds_menu(&mut self) {
        self.feeds_open = false;
        self.confirm_unsubscribe = None;
        self.feed_edit = None;
    }

    /// Closes the feeds dropdown and the ⋯ menu.
    fn close_overlays(&mut self) {
        self.close_feeds_menu();
        self.list_menu_open = false;
    }

    /// Unread articles in the current News scope.
    fn scope_unread(&self) -> u64 {
        let feeds = &self.sidebar.feeds;
        let in_scope = |f: FeedId| match self.scope {
            Scope::All => true,
            Scope::Feed(id) => f == id,
            Scope::Folder(id) => feeds.iter().any(|x| x.id == f && x.folder_id == Some(id)),
        };
        self.sidebar.unread.iter().filter(|(f, _)| in_scope(*f)).map(|(_, n)| n).sum()
    }

    /// Back to the first page of the reader list after a place/scope change.
    fn show_list(&mut self) -> Task<Message> {
        self.screen = Screen::Reader;
        self.page = Page::first(PAGE_SIZE);
        self.preview = None;
        self.load_list()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SidebarLoaded(Ok(sidebar)) => {
                self.sidebar = sidebar;
                Task::none()
            }
            Message::GoPlace(place) => {
                self.close_overlays();
                self.place = place;
                self.show_list()
            }
            Message::SetScope(scope) => {
                self.close_feeds_menu();
                self.place = Place::News;
                self.scope = scope;
                self.show_list()
            }
            Message::SetUnreadOnly(unread) => {
                if self.place != Place::News || unread == self.unread_only {
                    return Task::none();
                }
                self.unread_only = unread;
                self.show_list()
            }
            Message::ListLoaded(generation, Ok(listing)) => {
                if generation == self.list_generation {
                    self.listing = listing;
                }
                Task::none()
            }
            Message::Open(id) => {
                let db = self.db.clone();
                let was_unread = self.listing.items.iter().any(|a| a.id == id && !a.is_read);
                if let Some(item) = self.listing.items.iter_mut().find(|a| a.id == id) {
                    item.is_read = true;
                }
                let load = Task::perform(
                    async move {
                        db.call(move |s| -> juttusihti_core::Result<Box<Preview>> {
                            s.set_read(id, true)?;
                            let article = s.article(id)?;
                            let feed_title = s.feed(article.feed_id)?.title;
                            Ok(Box::new(Preview {
                                body: s.article_text(id)?,
                                labels: s.article_labels(id)?,
                                headline: s.headline(id)?,
                                feed_title,
                                article,
                            }))
                        })
                        .await
                    },
                    Message::PreviewLoaded,
                );
                if was_unread { Task::batch([load, self.load_sidebar()]) } else { load }
            }
            Message::PreviewLoaded(Ok(preview)) => {
                self.preview = Some(*preview);
                Task::none()
            }
            Message::ToggleSaved => {
                let Some(p) = self.preview.as_mut() else { return Task::none() };
                p.article.is_saved = !p.article.is_saved;
                let (id, saved) = (p.article.id, p.article.is_saved);
                if let Some(item) = self.listing.items.iter_mut().find(|a| a.id == id) {
                    item.is_saved = saved;
                }
                self.set_state(id, None, Some(saved))
            }
            Message::ToggleRead => {
                let Some(p) = self.preview.as_mut() else { return Task::none() };
                p.article.is_read = !p.article.is_read;
                let (id, read) = (p.article.id, p.article.is_read);
                if let Some(item) = self.listing.items.iter_mut().find(|a| a.id == id) {
                    item.is_read = read;
                }
                Task::batch([self.set_state(id, Some(read), None), self.load_sidebar()])
            }
            Message::OpenInBrowser => {
                if let Some(url) = self.preview.as_ref().and_then(|p| p.article.url.clone()) {
                    // Only http(s) URLs are ever stored; check again before handing to the OS.
                    if juttusihti_core::feed::is_web_url(&url)
                        && let Err(e) = open::that_detached(&url)
                    {
                        self.status = Some(format!("Could not open browser: {e}"));
                    }
                }
                Task::none()
            }
            Message::StateChanged(Ok(())) => Task::none(),
            Message::SearchInput(s) => {
                self.search = s;
                Task::none()
            }
            Message::SearchSubmit => {
                self.active_search = self.search.trim().to_string();
                self.page = Page::first(PAGE_SIZE);
                self.screen = Screen::Reader;
                self.load_list()
            }
            Message::ClearSearch => {
                self.search.clear();
                self.active_search.clear();
                self.page = Page::first(PAGE_SIZE);
                self.load_list()
            }
            Message::FocusSearch => operation::focus(SEARCH_ID),
            Message::PrevPage => {
                self.page = self.page.prev();
                self.load_list()
            }
            Message::NextPage => {
                if u64::from(self.page.offset + self.page.limit) < self.listing.total {
                    self.page = self.page.next();
                    return self.load_list();
                }
                Task::none()
            }
            Message::AddFeedInput(s) => {
                self.add_feed = s;
                Task::none()
            }
            Message::AddFeed => {
                let Some(fetcher) = self.fetcher.clone() else { return Task::none() };
                let input = self.add_feed.trim().to_string();
                if input.is_empty() {
                    return Task::none();
                }
                self.status = Some(format!("Looking for feeds at {input}…"));
                let db = self.db.clone();
                Task::perform(
                    async move {
                        let found = fetcher.discover(&input).await.map_err(|e| e.to_string())?;
                        let first = found.into_iter().next().ok_or_else(|| format!("No feed found at {input}"))?;
                        let title = first.title.clone().unwrap_or_default();
                        let url = first.url.clone();
                        let id = db
                            .call(move |s| s.add_feed(&NewFeed { url: first.url, title, ..Default::default() }))
                            .await?;
                        Ok((id, url))
                    },
                    Message::FeedAdded,
                )
            }
            Message::FeedAdded(Ok((id, url))) => {
                self.add_feed.clear();
                self.status = Some(format!("Subscribed to {url}"));
                let refresh = self.load_sidebar().chain(Task::done(Message::SetScope(Scope::Feed(id))));
                let feed = Feed {
                    id,
                    url,
                    title: String::new(),
                    site_url: None,
                    folder_id: None,
                    etag: None,
                    last_modified: None,
                    last_fetched_at: None,
                    last_error: None,
                };
                let fetch = self.refresh_feed(&feed).unwrap_or_else(Task::none);
                self.refreshing += 1;
                Task::batch([refresh, fetch])
            }
            Message::RemoveFeed(id) => {
                self.list_menu_open = false;
                self.confirm_unsubscribe = Some(id);
                self.feeds_open = true;
                Task::none()
            }
            Message::CancelUnsubscribe => {
                self.confirm_unsubscribe = None;
                Task::none()
            }
            Message::ConfirmUnsubscribe => {
                let Some(id) = self.confirm_unsubscribe.take() else { return Task::none() };
                if self.feed_edit.as_ref().is_some_and(|e| e.id == id) {
                    self.feed_edit = None;
                }
                let db = self.db.clone();
                let remove =
                    Task::perform(async move { db.call(move |s| s.remove_feed(id)).await }, Message::StateChanged)
                        .chain(self.load_sidebar());
                if self.scope != Scope::Feed(id) {
                    // Another feed: stay where we are, minus that feed's articles.
                    if self.preview.as_ref().is_some_and(|p| p.article.feed_id == id) {
                        self.preview = None;
                    }
                    return remove.chain(self.load_list());
                }
                self.place = Place::News;
                self.scope = Scope::All;
                self.preview = None;
                remove.chain(Task::done(Message::SetScope(Scope::All)))
            }
            Message::ToggleFeedsMenu => {
                if self.feeds_open {
                    self.close_feeds_menu();
                } else {
                    self.list_menu_open = false;
                    self.feeds_open = true;
                }
                Task::none()
            }
            Message::ToggleListMenu => {
                let open = !self.list_menu_open;
                self.close_overlays();
                self.list_menu_open = open;
                Task::none()
            }
            Message::CloseOverlays => {
                self.close_overlays();
                Task::none()
            }
            Message::MarkAllRead => {
                self.list_menu_open = false;
                let db = self.db.clone();
                let filter = self.list_filter();
                for item in &mut self.listing.items {
                    item.is_read = true;
                }
                Task::perform(
                    async move { db.call(move |s| s.mark_all_read(filter).map(|_| ())).await },
                    Message::StateChanged,
                )
                .chain(self.load_sidebar())
            }
            Message::RefreshAll | Message::AutoRefresh => {
                if self.refreshing > 0 {
                    return Task::none();
                }
                self.refresh_all()
            }
            Message::Refreshed(id, result) => {
                self.refreshing = self.refreshing.saturating_sub(1);
                if let Err(e) = &result {
                    let name = self.sidebar.feeds.iter().find(|f| f.id == id).map_or("feed", |f| f.title.as_str());
                    tracing::warn!(feed = id, error = %e, "refresh failed");
                    self.status = Some(format!("{name}: {e}"));
                }
                if self.refreshing == 0 {
                    return Task::batch([self.load_sidebar(), self.load_list()]);
                }
                Task::none()
            }
            Message::ImportOpml => {
                let db = self.db.clone();
                Task::perform(
                    async move {
                        let Some(file) =
                            rfd::AsyncFileDialog::new().add_filter("OPML", &["opml", "xml"]).pick_file().await
                        else {
                            return Ok((0, 0));
                        };
                        let bytes = file.read().await;
                        if bytes.len() > juttusihti_core::opml_io::MAX_OPML_BYTES {
                            return Err("OPML file is too large".to_string());
                        }
                        let xml = String::from_utf8_lossy(&bytes).into_owned();
                        db.call(move |s| juttusihti_core::opml_io::import(s, &xml)).await
                    },
                    Message::Imported,
                )
            }
            Message::Imported(Ok((in_file, added))) => {
                if in_file > 0 {
                    self.status = Some(format!("Imported {added} new of {in_file} subscriptions"));
                }
                self.load_sidebar().chain(Task::done(Message::RefreshAll))
            }
            Message::ExportOpml => {
                let db = self.db.clone();
                Task::perform(
                    async move {
                        let xml = db.call(|s| juttusihti_core::opml_io::export(s)).await?;
                        let Some(file) = rfd::AsyncFileDialog::new()
                            .add_filter("OPML", &["opml"])
                            .set_file_name("juttusihti.opml")
                            .save_file()
                            .await
                        else {
                            return Ok(None);
                        };
                        file.write(xml.as_bytes()).await.map_err(|e| e.to_string())?;
                        Ok(Some(file.path().to_path_buf()))
                    },
                    Message::Exported,
                )
            }
            Message::Exported(Ok(path)) => {
                if let Some(p) = path {
                    self.status = Some(format!("Exported subscriptions to {}", p.display()));
                }
                Task::none()
            }
            Message::ToggleTheme => {
                let dark = !self.settings.values.dark;
                self.settings.save_theme(dark).map(Message::Settings)
            }
            Message::ShowScreen(screen) => {
                self.close_overlays();
                self.screen = screen;
                match screen {
                    Screen::Reports => self.reports.load().map(Message::Reports),
                    Screen::Research => self.research.load().map(Message::Research),
                    Screen::Labels => self.labels.load().map(Message::Labels),
                    Screen::Review => self.review.load().map(Message::Review),
                    Screen::Graph => self.graph.load().map(Message::Graph),
                    Screen::Timeline => self.timeline.load().map(Message::Timeline),
                    Screen::Map => self.map.load().map(Message::Map),
                    Screen::Providers => self.providers.load().map(Message::Providers),
                    Screen::Settings => self.settings.load().map(Message::Settings),
                    Screen::Reader => Task::none(),
                }
            }
            Message::Reports(msg) => self.reports.update(msg).map(Message::Reports),
            Message::Research(crate::research::Msg::ReviewSession(id)) => {
                self.screen = Screen::Review;
                self.review.open(id).map(Message::Review)
            }
            Message::Research(msg) => self.research.update(msg).map(Message::Research),
            Message::Labels(msg) => self.labels.update(msg).map(Message::Labels),
            Message::Review(crate::review::Msg::OpenArticle(id)) => {
                self.screen = Screen::Reader;
                self.update(Message::Open(id))
            }
            Message::Review(msg) => self.review.update(msg).map(Message::Review),
            Message::Graph(crate::graph_view::Msg::OpenArticle(id))
            | Message::Timeline(crate::timeline_view::Msg::OpenArticle(id))
            | Message::Map(crate::map_view::Msg::OpenArticle(id)) => {
                self.screen = Screen::Reader;
                self.update(Message::Open(id))
            }
            Message::Graph(msg) => self.graph.update(msg).map(Message::Graph),
            Message::Timeline(msg) => self.timeline.update(msg).map(Message::Timeline),
            Message::Map(msg) => self.map.update(msg).map(Message::Map),
            Message::MapArticle => {
                let Some(p) = &self.preview else { return Task::none() };
                let (id, title) = (p.article.id, p.article.title.clone());
                self.screen = Screen::Graph;
                self.graph.focus_article(id, &title).map(Message::Graph)
            }
            Message::ToggleAiHeadlines => {
                self.list_menu_open = false;
                self.ai_headlines = !self.ai_headlines;
                Task::none()
            }
            Message::DecideHeadline(status) => {
                let Some(p) = self.preview.as_mut() else { return Task::none() };
                let Some(h) = p.headline.as_mut() else { return Task::none() };
                h.status = status;
                let id = p.article.id;
                match status {
                    juttusihti_core::Status::Rejected => {
                        self.listing.headlines.remove(&id);
                    }
                    _ => {
                        self.listing.headlines.insert(id, h.headline.clone());
                    }
                }
                let db = self.db.clone();
                Task::perform(
                    async move { db.call(move |s| s.decide_headline(id, status)).await },
                    Message::HeadlineDecided,
                )
            }
            Message::HeadlineDecided(r) => {
                if let Err(e) = r {
                    self.status = Some(e);
                }
                Task::none()
            }
            Message::ToggleHideDismissed => {
                self.list_menu_open = false;
                self.hide_dismissed = !self.hide_dismissed;
                self.page = Page::first(PAGE_SIZE);
                self.load_list()
            }
            Message::Providers(msg) => self.providers.update(msg).map(Message::Providers),
            Message::OpenQuick => {
                if self.quick.open && !self.quick.is_running() {
                    return self.quick.update(crate::quick::Msg::Close).map(Message::Quick);
                }
                self.quick.open(self.list_query(), self.scope_label()).map(Message::Quick)
            }
            Message::Quick(crate::quick::Msg::OpenSetup) => self.update(Message::ShowScreen(Screen::Providers)),
            Message::Quick(crate::quick::Msg::Review(id)) => {
                self.screen = Screen::Review;
                self.review.open(id).map(Message::Review)
            }
            Message::Quick(msg) => {
                // Show results as they arrive: rewritten headlines and labels.
                let refresh = matches!(
                    msg,
                    crate::quick::Msg::Run(crate::research::RunEvent::Progress(juttusihti_ai::Progress::Call { .. }))
                        | crate::quick::Msg::Run(crate::research::RunEvent::Finished(_))
                );
                let task = self.quick.update(msg).map(Message::Quick);
                if refresh { Task::batch([task, self.load_list(), self.load_sidebar()]) } else { task }
            }
            Message::Settings(msg) => {
                // Apply retention once settings are known at startup, and
                // reload lists after a save (retention may remove articles).
                let follow_up = match &msg {
                    crate::settings::Msg::Loaded(Ok(v)) => match v.retention_days {
                        Some(days) => {
                            let db = self.db.clone();
                            Task::perform(
                                async move { db.call(move |s| s.apply_retention(days)).await },
                                Message::RetentionApplied,
                            )
                        }
                        None => Task::none(),
                    },
                    crate::settings::Msg::Saved(Ok(_)) => Task::batch([self.load_sidebar(), self.load_list()]),
                    _ => Task::none(),
                };
                Task::batch([self.settings.update(msg).map(Message::Settings), follow_up])
            }
            Message::RetentionApplied(Ok(n)) => {
                if n > 0 {
                    tracing::info!(removed = n, "retention applied");
                    return Task::batch([self.load_sidebar(), self.load_list()]);
                }
                Task::none()
            }
            Message::LabelInput(s) => {
                self.label_input = s;
                Task::none()
            }
            Message::AddLabel => {
                let Some(id) = self.preview.as_ref().map(|p| p.article.id) else { return Task::none() };
                let name = self.label_input.trim().to_string();
                if name.is_empty() {
                    return Task::none();
                }
                self.label_input.clear();
                self.label_change(move |s| {
                    let label = s.create_label(&name)?;
                    s.tag_article(id, label, juttusihti_core::Origin::User, None)?;
                    Ok(id)
                })
            }
            Message::RemoveLabel(label) => {
                let Some(id) = self.preview.as_ref().map(|p| p.article.id) else { return Task::none() };
                self.label_change(move |s| s.untag_article(id, label).map(|_| id))
            }
            Message::DecideLabel(label, status) => {
                let Some(id) = self.preview.as_ref().map(|p| p.article.id) else { return Task::none() };
                self.label_change(move |s| s.decide_label(id, label, status).map(|_| id))
            }
            Message::LabelsChanged(Ok(labels)) => {
                if let Some(p) = self.preview.as_mut() {
                    p.labels = labels;
                }
                Task::none()
            }
            Message::EditFeed(id) => {
                self.list_menu_open = false;
                self.feeds_open = true;
                self.confirm_unsubscribe = None;
                if let Some(f) = self.sidebar.feeds.iter().find(|f| f.id == id) {
                    self.feed_edit =
                        Some(FeedEdit { id, title: f.title.clone(), folder: f.folder_id, new_folder: String::new() });
                }
                Task::none()
            }
            Message::FeedTitle(s) => {
                if let Some(e) = self.feed_edit.as_mut() {
                    e.title = s;
                }
                Task::none()
            }
            Message::FeedFolder(FolderChoice(folder, _)) => {
                if let Some(e) = self.feed_edit.as_mut() {
                    e.folder = folder;
                    e.new_folder.clear();
                }
                Task::none()
            }
            Message::NewFolder(s) => {
                if let Some(e) = self.feed_edit.as_mut() {
                    e.new_folder = s;
                }
                Task::none()
            }
            Message::CancelEditFeed => {
                self.feed_edit = None;
                Task::none()
            }
            Message::SaveFeed => {
                let Some(edit) = self.feed_edit.take() else { return Task::none() };
                let db = self.db.clone();
                Task::perform(
                    async move {
                        db.call(move |s| -> juttusihti_core::Result<()> {
                            let folder = match edit.new_folder.trim() {
                                "" => edit.folder,
                                name => Some(s.create_folder(name)?),
                            };
                            s.update_feed(edit.id, &edit.title, folder)
                        })
                        .await
                    },
                    Message::FeedSaved,
                )
            }
            Message::FeedSaved(Ok(())) => self.load_sidebar(),
            Message::Key(key, modifiers) => self.on_key(key, modifiers),
            Message::DismissStatus => {
                self.status = None;
                Task::none()
            }
            Message::SidebarLoaded(Err(e))
            | Message::ListLoaded(_, Err(e))
            | Message::PreviewLoaded(Err(e))
            | Message::StateChanged(Err(e))
            | Message::FeedAdded(Err(e))
            | Message::Imported(Err(e))
            | Message::Exported(Err(e))
            | Message::RetentionApplied(Err(e))
            | Message::LabelsChanged(Err(e))
            | Message::FeedSaved(Err(e)) => {
                tracing::warn!(error = %e, "operation failed");
                self.status = Some(e);
                Task::none()
            }
        }
    }

    fn on_key(&mut self, key: Key, modifiers: keyboard::Modifiers) -> Task<Message> {
        if modifiers.command() {
            return match key.as_ref() {
                Key::Character("f") => self.update(Message::FocusSearch),
                Key::Character("r") => self.update(Message::RefreshAll),
                _ => Task::none(),
            };
        }
        match key.as_ref() {
            Key::Character("j") | Key::Named(keyboard::key::Named::ArrowDown) => self.open_relative(1),
            Key::Character("k") | Key::Named(keyboard::key::Named::ArrowUp) => self.open_relative(-1),
            Key::Character("s") => self.update(Message::ToggleSaved),
            Key::Character("m") => self.update(Message::ToggleRead),
            Key::Character("o") | Key::Named(keyboard::key::Named::Enter) => self.update(Message::OpenInBrowser),
            Key::Character("/") => self.update(Message::FocusSearch),
            Key::Character("n") => self.update(Message::NextPage),
            Key::Character("p") => self.update(Message::PrevPage),
            Key::Named(keyboard::key::Named::Escape) => self.update(Message::CloseOverlays),
            _ => Task::none(),
        }
    }

    // ---- view ----------------------------------------------------------

    pub fn view(&self) -> Element<'_, Message> {
        responsive(move |size| self.view_sized(size)).into()
    }

    /// The whole window; `size` places the ⋯ menu at the right edge of the article list.
    fn view_sized(&self, size: Size) -> Element<'_, Message> {
        // Two fifths of the width right of the rail and the two vertical rules.
        let list_width = ((size.width - rail::WIDTH - 2.0) * 2.0 / 5.0).floor().max(0.0);
        let main: Element<'_, Message> = match self.screen {
            Screen::Reader => row![
                container(self.view_list()).width(list_width),
                rule::vertical(1),
                container(self.view_preview()).width(Fill),
            ]
            .height(Fill)
            .into(),
            Screen::Reports => self.reports.view().map(Message::Reports),
            Screen::Research => self.research.view().map(Message::Research),
            Screen::Labels => self.labels.view().map(Message::Labels),
            Screen::Review => self.review.view().map(Message::Review),
            Screen::Graph => self.graph.view().map(Message::Graph),
            Screen::Timeline => self.timeline.view().map(Message::Timeline),
            Screen::Map => self.map.view().map(Message::Map),
            Screen::Providers => self.providers.view().map(Message::Providers),
            Screen::Settings => self.settings.view().map(Message::Settings),
        };
        let body = row![rail::view(self.place, self.screen), rule::vertical(1), main].height(Fill);
        let mut layout = column![self.view_top_bar(), rule::horizontal(1), body];
        if let Some(status) = &self.status {
            layout = layout.push(
                container(
                    row![
                        text(status).size(13).width(Fill),
                        button(text("✕").size(12)).style(button::text).on_press(Message::DismissStatus)
                    ]
                    .align_y(iced::Center),
                )
                .padding([4, 12])
                .style(container::secondary),
            );
        }
        // Both menus open just under the list header.
        let below_header = TOP_BAR_HEIGHT + 1.0 + LIST_HEADER_HEIGHT;
        if self.feeds_open && self.screen == Screen::Reader && self.place == Place::News {
            // Under the dropdown button at the left of the list header.
            let anchor = Point::new(rail::WIDTH + 1.0 + LIST_HEADER_PAD, below_header);
            let panel = feeds_menu::panel(
                &self.sidebar,
                self.scope,
                &self.add_feed,
                self.feed_edit.as_ref(),
                self.confirm_unsubscribe,
                self.fetcher.is_some(),
            );
            return style::overlay(layout, panel, anchor, Message::CloseOverlays);
        }
        if self.list_menu_open && self.screen == Screen::Reader {
            // Right-aligned under the ⋯ button at the right of the list header.
            let left = rail::WIDTH + 1.0;
            let x = (left + list_width - LIST_HEADER_PAD - LIST_MENU_WIDTH).max(left);
            return style::overlay(layout, self.view_list_menu(), Point::new(x, below_header), Message::CloseOverlays);
        }
        layout.into()
    }

    fn view_top_bar(&self) -> Element<'_, Message> {
        let brand = text("Juttusihti").size(SIZE_TITLE).font(Font { weight: font::Weight::Bold, ..Font::DEFAULT });
        let search = text_input("Search news…", &self.search)
            .id(SEARCH_ID)
            .on_input(Message::SearchInput)
            .on_submit(Message::SearchSubmit)
            .size(SIZE_BODY)
            .padding(SPACE[1])
            .width(Fill);
        let help = tooltip(
            icon(Icon::Help).style(style::faint),
            style::tip("Search: words, \"phrases\", OR, -exclude, prefix*, title:…"),
            tooltip::Position::Bottom,
        );
        let clear = (!self.active_search.is_empty()).then(|| {
            button(text("Clear").size(SIZE_SMALL))
                .padding([SPACE[0], SPACE[1] + 2.0])
                .style(style::quiet)
                .on_press(Message::ClearSearch)
        });
        let refresh_tip =
            if self.refreshing > 0 { format!("Refreshing {}", self.refreshing) } else { "Refresh all feeds".into() };
        let refresh = tooltip(
            button(icon(Icon::Refresh))
                .padding(SPACE[1])
                .style(style::icon)
                .on_press_maybe((self.refreshing == 0 && self.fetcher.is_some()).then_some(Message::RefreshAll)),
            style::tip(refresh_tip),
            tooltip::Position::Bottom,
        );
        let settings = tooltip(
            button(icon(Icon::Settings))
                .padding(SPACE[1])
                .style(style::icon)
                .on_press(Message::ShowScreen(Screen::Settings)),
            style::tip("Settings"),
            tooltip::Position::Bottom,
        );
        container(
            row![brand, search, help].push(clear).push(refresh).push(settings).spacing(SPACE[2]).align_y(iced::Center),
        )
        .padding([0.0, SPACE[2]])
        .center_y(TOP_BAR_HEIGHT)
        .into()
    }

    fn view_list(&self) -> Element<'_, Message> {
        let selected = self.preview.as_ref().map(|p| p.article.id);
        let mut list = column![].spacing(1).padding([SPACE[0], SPACE[1]]);
        for a in &self.listing.items {
            list = list.push(self.view_row(a, selected == Some(a.id)));
        }
        if self.listing.items.is_empty() {
            if self.place == Place::News && self.sidebar.feeds.is_empty() && self.active_search.is_empty() {
                list = list.push(
                    column![
                        text("No feeds yet.").size(SIZE_BODY).style(style::muted),
                        button(text("Add a site or feed").size(SIZE_BODY))
                            .padding([SPACE[1], SPACE[2]])
                            .style(style::main)
                            .on_press(Message::ToggleFeedsMenu),
                    ]
                    .spacing(SPACE[2])
                    .padding(SPACE[3]),
                );
            } else {
                let msg = if self.active_search.is_empty() { "No articles." } else { "No matches." };
                list = list.push(container(text(msg).size(SIZE_BODY).style(style::muted)).padding(SPACE[3]));
            }
        }

        let mut header = row![].spacing(SPACE[1]).align_y(Center);
        header = match self.place {
            Place::News => header
                .push(feeds_menu::button(self.scope_label(), self.scope_unread(), self.feeds_open))
                .push(style::segmented(&[(false, "All"), (true, "Unread")], self.unread_only, Message::SetUnreadOnly)),
            Place::Saved => {
                header.push(container(text("Saved").size(SIZE_BODY).font(SEMIBOLD)).padding([0.0, SPACE[1]]))
            }
        };
        header = header
            .push(Space::new().width(Fill))
            .push(header_icon(Icon::Sparkles, "AI for this list", self.quick.open, Message::OpenQuick))
            .push(header_icon(Icon::More, "More", self.list_menu_open, Message::ToggleListMenu));
        let mut col = column![container(header).padding([0.0, LIST_HEADER_PAD]).center_y(LIST_HEADER_HEIGHT)];
        if self.quick.open {
            col = col.push(container(self.quick.view().map(Message::Quick)).padding([SPACE[0], LIST_HEADER_PAD]));
        }
        col.push(rule::horizontal(1))
            .push(scrollable(list).height(Fill))
            .push(rule::horizontal(1))
            .push(self.view_list_footer())
            .into()
    }

    /// One article: unread dot, headline, "Source · time" and the search snippet.
    fn view_row<'a>(&'a self, a: &'a ArticleSummary, selected: bool) -> Element<'a, Message> {
        let rewritten = self.ai_headlines.then(|| self.listing.headlines.get(&a.id)).flatten();
        let font = if a.is_read { Font::DEFAULT } else { SEMIBOLD };
        let mut title = row![].spacing(SPACE[0] + 2.0);
        if rewritten.is_some() {
            title = title.push(tooltip(
                container(icon(Icon::Sparkles).size(SIZE_SMALL).style(style::faint)).padding(Padding::ZERO.top(3.0)),
                style::tip("AI-rewritten headline"),
                tooltip::Position::Top,
            ));
        }
        title = title.push(text(rewritten.unwrap_or(&a.title)).size(SIZE_BODY).font(font));

        let mut meta = row![
            text(format!("{} · {}", a.feed_title, format::list_date(a.effective_at)))
                .size(SIZE_SMALL)
                .style(style::faint)
        ]
        .spacing(SPACE[0] + 2.0)
        .align_y(Center);
        if a.is_saved {
            meta = meta.push(icon(Icon::Star).size(SIZE_SMALL).style(style::accent));
        }
        let mut entry = column![title, meta].spacing(2).width(Fill);
        if let Some(snippet) = &a.snippet {
            let spans: Vec<_> = format::snippet_segments(snippet)
                .into_iter()
                .map(|(t, hl)| {
                    let s: iced::widget::text::Span<'_, ()> = span(t);
                    if hl { s.font(SEMIBOLD).underline(true) } else { s }
                })
                .collect();
            entry = entry.push(rich_text(spans).size(SIZE_SMALL).style(style::muted));
        }

        // Unread dot, centred on the first headline line.
        let dot: Element<'_, Message> = if a.is_read {
            Space::new().width(UNREAD_DOT).into()
        } else {
            container(Space::new().width(UNREAD_DOT).height(UNREAD_DOT)).style(style::unread_dot).into()
        };
        let dot = container(dot).padding(Padding::ZERO.top((SIZE_BODY * 1.3 - UNREAD_DOT) / 2.0));
        let row_button = button(row![dot, entry].spacing(SPACE[1]))
            .width(Fill)
            .padding([SPACE[1], SPACE[2]])
            .style(if selected { style::row_selected } else { style::row })
            .on_press(Message::Open(a.id));
        if selected {
            let edge = container(Space::new().width(2).height(Fill)).style(style::accent_edge);
            stack![row_button, edge].width(Fill).into()
        } else {
            row_button.into()
        }
    }

    /// Range, page arrows and notes about the search under the list.
    fn view_list_footer(&self) -> Element<'_, Message> {
        let first = self.page.offset + 1;
        let last = u64::from(self.page.offset) + self.listing.items.len() as u64;
        let range = if self.listing.total == 0 {
            "0".to_string()
        } else {
            let plus = if self.listing.total_is_exact { "" } else { "+" };
            format!("{first}–{last} of {}{plus}", self.listing.total)
        };
        let has_next = u64::from(self.page.offset + self.page.limit) < self.listing.total;
        let page_button = |i: Icon, tip: &'static str, target: Option<Message>| {
            tooltip(
                button(icon(i).size(SIZE_BODY)).padding(SPACE[0]).style(style::icon).on_press_maybe(target),
                style::tip(tip),
                tooltip::Position::Top,
            )
        };
        let mut footer = column![].spacing(SPACE[0]);
        if self.listing.ranked_window {
            footer = footer.push(
                text("Very many matches: ranked among the 10,000 newest. Add words or filters to narrow.")
                    .size(SIZE_SMALL)
                    .style(style::faint),
            );
        }
        if let Some(interp) = &self.listing.interpreted {
            footer = footer.push(text(format!("Query: {interp}")).size(SIZE_SMALL).style(style::faint));
        }
        footer = footer.push(
            row![
                text(range).size(SIZE_SMALL).style(style::faint).width(Fill),
                page_button(
                    Icon::ChevronLeft,
                    "Previous page (p)",
                    (self.page.offset > 0).then_some(Message::PrevPage)
                ),
                page_button(Icon::ChevronRight, "Next page (n)", has_next.then_some(Message::NextPage)),
            ]
            .spacing(SPACE[0])
            .align_y(Center),
        );
        container(footer).padding([SPACE[0], LIST_HEADER_PAD]).into()
    }

    /// The ⋯ menu: list-wide actions and, for a single feed, edit and unsubscribe.
    fn view_list_menu(&self) -> Element<'_, Message> {
        let item = |label: String, target: Message| -> Element<'_, Message> {
            button(text(label).size(SIZE_BODY))
                .width(Fill)
                .padding([SPACE[0] + 2.0, SPACE[1]])
                .style(style::row)
                .on_press(target)
                .into()
        };
        let dismissed = if self.hide_dismissed { "Show dismissed" } else { "Hide dismissed" };
        let mut menu = column![
            item("Mark all read".into(), Message::MarkAllRead),
            item(dismissed.into(), Message::ToggleHideDismissed),
        ]
        .spacing(2);
        // Only offered when this page has rewrites to switch between.
        let rewritten = self.listing.headlines.len();
        if rewritten > 0 {
            let label = if self.ai_headlines {
                format!("Show original headlines ({rewritten} rewritten)")
            } else {
                format!("Show AI headlines ({rewritten})")
            };
            menu = menu.push(item(label, Message::ToggleAiHeadlines));
        }
        if let (Place::News, Scope::Feed(id)) = (self.place, self.scope) {
            menu = menu.push(rule::horizontal(1)).push(item("Edit feed".into(), Message::EditFeed(id))).push(
                container(quiet_button(None, "Unsubscribe…", style::quiet_danger, Some(Message::RemoveFeed(id))))
                    .padding([SPACE[0], SPACE[1]]),
            );
        }
        container(menu).width(LIST_MENU_WIDTH).padding(SPACE[1]).style(style::popover).into()
    }

    fn label_change(
        &self,
        f: impl FnOnce(&mut juttusihti_core::store::Store) -> juttusihti_core::Result<ArticleId> + Send + 'static,
    ) -> Task<Message> {
        let db = self.db.clone();
        Task::perform(
            async move {
                db.call(move |s| -> juttusihti_core::Result<Vec<ArticleLabel>> {
                    let id = f(s)?;
                    s.article_labels(id)
                })
                .await
            },
            Message::LabelsChanged,
        )
    }

    /// The article title, or its AI rewrite with the original underneath
    /// and buttons to keep or reject the rewrite.
    fn view_headline<'a>(&'a self, p: &'a Preview) -> Element<'a, Message> {
        use juttusihti_core::Status;
        let original = &p.article.title;
        match &p.headline {
            Some(h) if self.ai_headlines && h.status != Status::Rejected => {
                let mut note = row![
                    icon(Icon::Sparkles).size(SIZE_SMALL).style(style::faint),
                    text("AI-rewritten headline").size(SIZE_SMALL).style(style::faint),
                    Space::new().width(SPACE[0]),
                ]
                .spacing(SPACE[0])
                .align_y(Center);
                if h.status == Status::Suggested {
                    let keep = Some(Message::DecideHeadline(Status::Confirmed));
                    note = note.push(quiet_button(None, "Keep", style::quiet, keep));
                }
                let reject = Some(Message::DecideHeadline(Status::Rejected));
                note = note.push(quiet_button(None, "Use original", style::quiet, reject));
                column![
                    text(&h.headline).size(SIZE_ARTICLE_TITLE).font(SEMIBOLD),
                    text(format!("Original: {original}")).size(SIZE_SMALL).style(style::muted),
                    note,
                ]
                .spacing(SPACE[1])
                .into()
            }
            _ => text(original).size(SIZE_ARTICLE_TITLE).font(SEMIBOLD).into(),
        }
    }

    fn view_preview(&self) -> Element<'_, Message> {
        let Some(p) = &self.preview else {
            return container(
                column![
                    text("Select an article").size(SIZE_BODY).style(style::faint),
                    text("j/k move · s save · m read/unread · o open · / search").size(SIZE_SMALL).style(style::faint),
                ]
                .spacing(SPACE[1])
                .align_x(Center),
            )
            .center(Fill)
            .into();
        };
        let a = &p.article;
        let mut meta = format!("{} · {}", p.feed_title, format::full_date(a.effective_at));
        if a.published_at.is_none() {
            meta.push_str(" (date fetched; feed gave no date)");
        }
        if let Some(author) = &a.author {
            meta.push_str(&format!(" · {author}"));
        }
        let save = if a.is_saved {
            quiet_button(Some(Icon::Star), "Saved", style::quiet_active, Some(Message::ToggleSaved))
        } else {
            quiet_button(Some(Icon::Star), "Save", style::quiet, Some(Message::ToggleSaved))
        };
        let open = a.url.as_ref().map(|_| Message::OpenInBrowser);
        let read = if a.is_read { "Mark unread" } else { "Mark read" };
        let actions = row![
            save,
            quiet_button(Some(Icon::External), "Open in browser", style::quiet, open),
            quiet_button(None, read, style::quiet, Some(Message::ToggleRead)),
            tooltip(
                quiet_button(Some(Icon::Connections), "Connections", style::quiet, Some(Message::MapArticle)),
                style::tip("Relationship map around this article"),
                tooltip::Position::Bottom,
            ),
        ]
        .spacing(SPACE[1])
        .wrap();

        let mut labels = row![].spacing(SPACE[1]).align_y(Center);
        for l in &p.labels {
            let marker = match l.status {
                juttusihti_core::Status::Confirmed => "",
                juttusihti_core::Status::Suggested => " (AI suggestion)",
                juttusihti_core::Status::Rejected => continue,
            };
            let id = l.label.id;
            let chip_button = |glyph: &'static str, target: Message| {
                button(text(glyph).size(SIZE_SMALL)).padding(0).style(style::icon).on_press(target)
            };
            let mut chip =
                row![text(format!("{}{marker}", l.label.name)).size(SIZE_SMALL)].spacing(SPACE[0]).align_y(Center);
            if l.status == juttusihti_core::Status::Suggested {
                chip = chip
                    .push(chip_button("✓", Message::DecideLabel(id, juttusihti_core::Status::Confirmed)))
                    .push(chip_button("✕", Message::DecideLabel(id, juttusihti_core::Status::Rejected)));
            } else {
                chip = chip.push(chip_button("×", Message::RemoveLabel(id)));
            }
            labels = labels.push(container(chip).padding([2.0, SPACE[1]]).style(style::chip));
        }
        labels = labels.push(
            text_input("+ label", &self.label_input)
                .on_input(Message::LabelInput)
                .on_submit(Message::AddLabel)
                .size(SIZE_SMALL)
                .padding([SPACE[0], SPACE[1]])
                .width(140),
        );
        let body = if p.body.is_empty() {
            "This entry has no text in the feed. Open it in the browser to read it."
        } else {
            &p.body
        };
        scrollable(
            column![
                column![text(meta).size(SIZE_SMALL).style(style::muted), self.view_headline(p)].spacing(SPACE[1]),
                actions,
                text(body).size(SIZE_BODY).line_height(1.6).style(style::body),
                rule::horizontal(1),
                column![text("Labels").size(SIZE_SMALL).style(style::faint), labels.wrap()].spacing(SPACE[1]),
            ]
            .spacing(SPACE[4])
            .padding(SPACE[4])
            .max_width(READ_WIDTH + 2.0 * SPACE[4]),
        )
        .height(Fill)
        .into()
    }
}

/// An icon button in the list header; `open` shows its panel is up.
fn header_icon<'a>(i: Icon, tip: &'a str, open: bool, target: Message) -> Element<'a, Message> {
    tooltip(
        button(icon(i))
            .padding(SPACE[0] + 2.0)
            .style(if open { style::row_selected } else { style::icon })
            .on_press(target),
        style::tip(tip),
        tooltip::Position::Bottom,
    )
    .into()
}

/// A quiet button with small text, optionally led by an icon in the same colour.
fn quiet_button<'a>(
    lead: Option<Icon>,
    label: &'a str,
    style: fn(&Theme, button::Status) -> button::Style,
    target: Option<Message>,
) -> Element<'a, Message> {
    let mut content = row![].spacing(SPACE[0] + 2.0).align_y(Center);
    if let Some(i) = lead {
        content = content.push(icon(i).size(SIZE_SMALL));
    }
    content = content.push(text(label).size(SIZE_SMALL));
    button(content).padding([SPACE[0], SPACE[1] + 2.0]).style(style).on_press_maybe(target).into()
}

#[cfg(test)]
mod tests {
    use juttusihti_core::store::Store;

    use super::*;

    fn app_with_feed() -> (App, FeedId) {
        let store = Store::open_in_memory().unwrap();
        let feed = store
            .add_feed(&NewFeed {
                url: "https://example.com/feed.xml".into(),
                title: "Example".into(),
                ..Default::default()
            })
            .unwrap();
        let (app, _task) = App::boot(Db::new(store), PathBuf::from("test.db"));
        (app, feed)
    }

    impl App {
        fn db_has_feed(&self, id: FeedId) -> bool {
            self.db.shared().lock().unwrap().feeds().unwrap().iter().any(|f| f.id == id)
        }
    }

    #[test]
    fn unsubscribe_asks_first() {
        let (mut app, feed) = app_with_feed();
        let _ = app.update(Message::RemoveFeed(feed));
        assert_eq!(app.confirm_unsubscribe, Some(feed));
        assert!(app.db_has_feed(feed));
        let _ = app.update(Message::CancelUnsubscribe);
        assert_eq!(app.confirm_unsubscribe, None);
    }

    #[test]
    fn unsubscribing_current_feed_falls_back_to_all() {
        let (mut app, feed) = app_with_feed();
        let _ = app.update(Message::SetScope(Scope::Feed(feed)));
        let _ = app.update(Message::RemoveFeed(feed));
        let _ = app.update(Message::ConfirmUnsubscribe);
        assert_eq!(app.scope, Scope::All);
    }

    #[test]
    fn unread_toggle_is_news_only() {
        let (mut app, _) = app_with_feed();
        let _ = app.update(Message::GoPlace(Place::Saved));
        let _ = app.update(Message::SetUnreadOnly(true));
        assert!(!app.unread_only);
    }

    #[test]
    fn unread_segments_set_rather_than_toggle() {
        let (mut app, _) = app_with_feed();
        let _ = app.update(Message::SetUnreadOnly(true));
        let _ = app.update(Message::SetUnreadOnly(true));
        assert!(app.unread_only);
        let _ = app.update(Message::SetUnreadOnly(false));
        assert!(!app.unread_only);
    }

    #[test]
    fn list_menu_toggles_and_closes_on_escape() {
        let (mut app, _) = app_with_feed();
        let _ = app.update(Message::ToggleListMenu);
        assert!(app.list_menu_open);
        let _ = app.update(Message::ToggleListMenu);
        assert!(!app.list_menu_open);
        let _ = app.update(Message::ToggleListMenu);
        let _ = app.update(Message::Key(Key::Named(keyboard::key::Named::Escape), Default::default()));
        assert!(!app.list_menu_open);
    }

    #[test]
    fn changing_place_or_screen_closes_menus() {
        let (mut app, _) = app_with_feed();
        let _ = app.update(Message::ToggleFeedsMenu);
        let _ = app.update(Message::ShowScreen(Screen::Reports));
        assert!(!app.feeds_open);
        let _ = app.update(Message::ToggleListMenu);
        let _ = app.update(Message::GoPlace(Place::Saved));
        assert!(!app.list_menu_open);
    }

    #[test]
    fn escape_closes_overlays() {
        let (mut app, _) = app_with_feed();
        let _ = app.update(Message::ToggleFeedsMenu);
        let _ = app.update(Message::Key(Key::Named(keyboard::key::Named::Escape), Default::default()));
        assert!(!app.feeds_open);
    }

    #[test]
    fn news_combines_scope_and_unread() {
        let (mut app, feed) = app_with_feed();
        let _ = app.update(Message::SetScope(Scope::Feed(feed)));
        let _ = app.update(Message::SetUnreadOnly(true));
        assert_eq!(app.list_filter(), ListFilter { view: View::Unread, feed_id: Some(feed), folder_id: None });
    }

    #[test]
    fn saved_ignores_scope_and_news_remembers_it() {
        let (mut app, feed) = app_with_feed();
        let _ = app.update(Message::SetScope(Scope::Feed(feed)));
        let _ = app.update(Message::GoPlace(Place::Saved));
        assert_eq!(app.list_filter(), ListFilter { view: View::Saved, ..Default::default() });
        let _ = app.update(Message::ShowScreen(Screen::Reports));
        let _ = app.update(Message::GoPlace(Place::News));
        assert_eq!(app.list_filter().feed_id, Some(feed));
    }
}

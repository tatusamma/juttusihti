//! Application state, messages, update and view.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iced::keyboard::{self, Key};
use iced::widget::{
    Space, button, column, container, operation, pick_list, rich_text, row, rule, scrollable, span, text, text_input,
};
use iced::{Element, Fill, Font, Length, Subscription, Task, Theme, font};
use juttusihti_analytics::{AnalyticsWorker, Limits};
use juttusihti_core::search::SearchQuery;
use juttusihti_core::store::ListFilter;
use juttusihti_core::{
    Article, ArticleId, ArticleLabel, ArticleSummary, Feed, FeedId, Folder, FolderId, NewFeed, Page, View,
};
use juttusihti_fetch::{Fetcher, Policy};
use tokio::sync::Semaphore;

use crate::db::Db;
use crate::format;

const PAGE_SIZE: u32 = 50;
const MAX_PARALLEL_FETCHES: usize = 4;
const SEARCH_ID: &str = "search";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    View(View),
    Feed(FeedId),
    Folder(FolderId),
}

impl Selection {
    fn filter(self) -> ListFilter {
        match self {
            Selection::View(view) => ListFilter { view, ..Default::default() },
            Selection::Feed(id) => ListFilter { feed_id: Some(id), ..Default::default() },
            Selection::Folder(id) => ListFilter { folder_id: Some(id), ..Default::default() },
        }
    }
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
    feeds: Vec<Feed>,
    folders: Vec<Folder>,
    unread: Vec<(FeedId, u64)>,
}

#[derive(Debug, Clone)]
pub struct Listing {
    items: Vec<ArticleSummary>,
    total: u64,
    interpreted: Option<String>,
}

/// Feed being edited in the list header.
#[derive(Debug, Clone)]
pub struct FeedEdit {
    id: FeedId,
    title: String,
    folder: Option<FolderId>,
    new_folder: String,
}

/// Pick-list entry for a feed's folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderChoice(Option<FolderId>, String);

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
}

#[derive(Debug, Clone)]
pub enum Message {
    SidebarLoaded(Result<Sidebar, String>),
    Select(Selection),
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
    RemoveFeed(FeedId),
    MarkAllRead,
    RefreshAll,
    Refreshed(FeedId, Result<usize, String>),
    AutoRefresh,
    ImportOpml,
    Imported(Result<(usize, usize), String>),
    ExportOpml,
    Exported(Result<Option<PathBuf>, String>),
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
    Providers(crate::providers::Msg),
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
    selection: Selection,
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
    reports: crate::reports::State,
    research: crate::research::State,
    labels: crate::labels::State,
    review: crate::review::State,
    graph: crate::graph_view::State,
    timeline: crate::timeline_view::State,
    map: crate::map_view::State,
    hide_dismissed: bool,
    providers: crate::providers::State,
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
            providers: crate::providers::State::new(),
            settings: crate::settings::State::new(db.clone(), db_path.clone()),
            db,
            fetcher,
            fetch_slots: Arc::new(Semaphore::new(MAX_PARALLEL_FETCHES)),
            sidebar: Sidebar { feeds: vec![], folders: vec![], unread: vec![] },
            selection: Selection::View(View::All),
            screen: Screen::Reader,
            search: String::new(),
            active_search: String::new(),
            page: Page::first(PAGE_SIZE),
            list_generation: 0,
            listing: Listing { items: vec![], total: 0, interpreted: None },
            preview: None,
            add_feed: String::new(),
            refreshing: 0,
            status,
            label_input: String::new(),
            feed_edit: None,
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
        if self.settings.values.dark { Theme::TokyoNight } else { Theme::Light }
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

    fn load_list(&mut self) -> Task<Message> {
        self.list_generation += 1;
        let generation = self.list_generation;
        let db = self.db.clone();
        let filter = self.selection.filter();
        let page = self.page;
        let query = SearchQuery {
            text: self.active_search.clone(),
            view: filter.view,
            feed_ids: filter.feed_id.into_iter().collect(),
            folder_ids: filter.folder_id.into_iter().collect(),
            hide_dismissed: self.hide_dismissed,
            ..Default::default()
        };
        Task::perform(
            async move {
                db.call(move |s| {
                    s.search(&query, page).map(|r| Listing {
                        items: r.items,
                        total: r.total,
                        interpreted: r.interpreted,
                    })
                })
                .await
            },
            move |r| Message::ListLoaded(generation, r),
        )
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

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SidebarLoaded(Ok(sidebar)) => {
                self.sidebar = sidebar;
                Task::none()
            }
            Message::Select(selection) => {
                self.selection = selection;
                self.screen = Screen::Reader;
                self.page = Page::first(PAGE_SIZE);
                self.preview = None;
                self.load_list()
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
                let refresh = self.load_sidebar().chain(Task::done(Message::Select(Selection::Feed(id))));
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
                let db = self.db.clone();
                self.selection = Selection::View(View::All);
                self.preview = None;
                Task::perform(async move { db.call(move |s| s.remove_feed(id)).await }, Message::StateChanged)
                    .chain(self.load_sidebar())
                    .chain(Task::done(Message::Select(Selection::View(View::All))))
            }
            Message::MarkAllRead => {
                let db = self.db.clone();
                let filter = self.selection.filter();
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
            Message::ToggleHideDismissed => {
                self.hide_dismissed = !self.hide_dismissed;
                self.page = Page::first(PAGE_SIZE);
                self.load_list()
            }
            Message::Providers(msg) => self.providers.update(msg).map(Message::Providers),
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
            _ => Task::none(),
        }
    }

    // ---- view ----------------------------------------------------------

    pub fn view(&self) -> Element<'_, Message> {
        let main: Element<'_, Message> = match self.screen {
            Screen::Reader => row![
                container(self.view_list()).width(Length::FillPortion(2)),
                rule::vertical(1),
                container(self.view_preview()).width(Length::FillPortion(3)),
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
        let body = row![container(self.view_sidebar()).width(260), rule::vertical(1), main].height(Fill);
        let mut layout = column![self.view_toolbar(), rule::horizontal(1), body];
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
        layout.into()
    }

    fn view_toolbar(&self) -> Element<'_, Message> {
        let search = text_input("Search: words, \"phrases\", OR, -exclude, prefix*, title:…", &self.search)
            .id(SEARCH_ID)
            .on_input(Message::SearchInput)
            .on_submit(Message::SearchSubmit)
            .padding(8)
            .width(Fill);
        let clear = (!self.active_search.is_empty())
            .then(|| button(text("Clear")).style(button::secondary).on_press(Message::ClearSearch));
        let refresh_label =
            if self.refreshing > 0 { format!("Refreshing ({})", self.refreshing) } else { "Refresh".into() };
        let refresh = button(text(refresh_label))
            .on_press_maybe((self.refreshing == 0 && self.fetcher.is_some()).then_some(Message::RefreshAll));
        let theme = button(text(if self.settings.values.dark { "Light" } else { "Dark" }))
            .style(button::secondary)
            .on_press(Message::ToggleTheme);
        row![text("Juttusihti").size(20).font(Font { weight: font::Weight::Bold, ..Font::DEFAULT }), search]
            .push(clear)
            .push(refresh)
            .push(theme)
            .spacing(12)
            .padding(10)
            .align_y(iced::Center)
            .into()
    }

    fn nav_button<'a>(&self, label: String, count: Option<u64>, target: Selection) -> Element<'a, Message> {
        let selected = self.selection == target && self.screen == Screen::Reader;
        let content = row![text(label).size(14).width(Fill)]
            .push(count.filter(|c| *c > 0).map(|c| text(c.to_string()).size(12)))
            .align_y(iced::Center);
        button(content)
            .width(Fill)
            .padding([6, 10])
            .style(if selected { button::primary } else { button::text })
            .on_press(Message::Select(target))
            .into()
    }

    fn view_sidebar(&self) -> Element<'_, Message> {
        let unread_of = |id: FeedId| self.sidebar.unread.iter().find(|(f, _)| *f == id).map(|(_, n)| *n);
        let total_unread: u64 = self.sidebar.unread.iter().map(|(_, n)| n).sum();
        let mut nav = column![
            self.nav_button("All articles".into(), None, Selection::View(View::All)),
            self.nav_button("Unread".into(), Some(total_unread), Selection::View(View::Unread)),
            self.nav_button("Saved".into(), None, Selection::View(View::Saved)),
            button(text("Reports").size(14))
                .width(Fill)
                .padding([6, 10])
                .style(if self.screen == Screen::Reports { button::primary } else { button::text })
                .on_press(Message::ShowScreen(Screen::Reports)),
            button(text("New Analysis").size(14))
                .width(Fill)
                .padding([6, 10])
                .style(if self.screen == Screen::Research { button::primary } else { button::text })
                .on_press(Message::ShowScreen(Screen::Research)),
            self.screen_button("Review", Screen::Review),
            self.screen_button("Relationship map", Screen::Graph),
            self.screen_button("Timeline", Screen::Timeline),
            self.screen_button("Map", Screen::Map),
            self.screen_button("Labels", Screen::Labels),
            self.screen_button("AI Providers", Screen::Providers),
            self.screen_button("Settings", Screen::Settings),
            rule::horizontal(1),
        ]
        .spacing(2);

        for folder in &self.sidebar.folders {
            let feeds: Vec<&Feed> = self.sidebar.feeds.iter().filter(|f| f.folder_id == Some(folder.id)).collect();
            let unread: u64 = feeds.iter().filter_map(|f| unread_of(f.id)).sum();
            nav = nav.push(self.nav_button(format!("▾ {}", folder.name), Some(unread), Selection::Folder(folder.id)));
            for f in feeds {
                nav = nav.push(
                    container(self.feed_button(f, unread_of(f.id)))
                        .padding(iced::Padding { left: 12.0, ..Default::default() }),
                );
            }
        }
        for f in self.sidebar.feeds.iter().filter(|f| f.folder_id.is_none()) {
            nav = nav.push(self.feed_button(f, unread_of(f.id)));
        }
        if self.sidebar.feeds.is_empty() {
            nav = nav.push(text("No subscriptions yet. Add a site or feed URL below, or import OPML.").size(13));
        }

        let add = column![
            text_input("Site or feed URL", &self.add_feed)
                .on_input(Message::AddFeedInput)
                .on_submit(Message::AddFeed)
                .padding(6),
            row![
                button(text("Add").size(13)).on_press_maybe(self.fetcher.as_ref().map(|_| Message::AddFeed)),
                button(text("Import").size(13)).style(button::secondary).on_press(Message::ImportOpml),
                button(text("Export").size(13)).style(button::secondary).on_press(Message::ExportOpml),
            ]
            .spacing(6),
        ]
        .spacing(6);

        column![scrollable(nav.padding(8)).height(Fill), rule::horizontal(1), container(add).padding(8)].into()
    }

    fn feed_button<'a>(&self, f: &Feed, unread: Option<u64>) -> Element<'a, Message> {
        let label = if f.last_error.is_some() { format!("⚠ {}", f.title) } else { f.title.clone() };
        self.nav_button(label, unread, Selection::Feed(f.id))
    }

    fn view_list(&self) -> Element<'_, Message> {
        let selected = self.preview.as_ref().map(|p| p.article.id);
        let mut list = column![].spacing(1);
        for a in &self.listing.items {
            let weight = if a.is_read { font::Weight::Normal } else { font::Weight::Bold };
            let mut entry = column![
                text(&a.title).size(15).font(Font { weight, ..Font::DEFAULT }),
                row![
                    text(&a.feed_title).size(12).width(Fill),
                    text(if a.is_saved { "★ " } else { "" }).size(12),
                    text(format::list_date(a.effective_at)).size(12),
                ],
            ]
            .spacing(2);
            if let Some(snippet) = &a.snippet {
                let spans: Vec<_> = format::snippet_segments(snippet)
                    .into_iter()
                    .map(|(t, hl)| {
                        let s: iced::widget::text::Span<'_, ()> = span(t).size(12);
                        if hl {
                            s.font(Font { weight: font::Weight::Bold, ..Font::DEFAULT }).underline(true)
                        } else {
                            s
                        }
                    })
                    .collect();
                entry = entry.push(rich_text(spans));
            }
            list = list.push(
                button(entry)
                    .width(Fill)
                    .padding([8, 12])
                    .style(if selected == Some(a.id) { button::secondary } else { button::text })
                    .on_press(Message::Open(a.id)),
            );
        }
        if self.listing.items.is_empty() {
            let msg = if self.active_search.is_empty() { "No articles." } else { "No matches." };
            list = list.push(container(text(msg)).padding(16));
        }

        let first = self.page.offset + 1;
        let last = u64::from(self.page.offset) + self.listing.items.len() as u64;
        let range = if self.listing.total == 0 {
            "0".to_string()
        } else {
            format!("{first}–{last} of {}", self.listing.total)
        };
        let mut header = row![text(range).size(12).width(Fill)].spacing(6).align_y(iced::Center);
        if let Some(interp) = &self.listing.interpreted {
            header = header.push(text(format!("Query: {interp}")).size(12));
        }
        header = header
            .push(
                button(text(if self.hide_dismissed { "Show dismissed" } else { "Hide dismissed" }).size(12))
                    .style(button::text)
                    .on_press(Message::ToggleHideDismissed),
            )
            .push(button(text("Mark all read").size(12)).style(button::text).on_press(Message::MarkAllRead))
            .push(
                button(text("‹").size(14))
                    .style(button::text)
                    .on_press_maybe((self.page.offset > 0).then_some(Message::PrevPage)),
            )
            .push(button(text("›").size(14)).style(button::text).on_press_maybe(
                (u64::from(self.page.offset + self.page.limit) < self.listing.total).then_some(Message::NextPage),
            ));
        if let Selection::Feed(id) = self.selection {
            header = header
                .push(button(text("Edit").size(12)).style(button::text).on_press(Message::EditFeed(id)))
                .push(button(text("Unsubscribe").size(12)).style(button::danger).on_press(Message::RemoveFeed(id)));
        }
        let mut col = column![container(header).padding([6, 12])];
        if let Some(edit) = self.feed_edit.as_ref().filter(|e| self.selection == Selection::Feed(e.id)) {
            col = col.push(self.view_feed_edit(edit));
        }
        col.push(rule::horizontal(1)).push(scrollable(list).height(Fill)).into()
    }

    fn screen_button<'a>(&self, label: &'a str, screen: Screen) -> Element<'a, Message> {
        button(text(label).size(14))
            .width(Fill)
            .padding([6, 10])
            .style(if self.screen == screen { button::primary } else { button::text })
            .on_press(Message::ShowScreen(screen))
            .into()
    }

    fn view_feed_edit<'a>(&'a self, edit: &'a FeedEdit) -> Element<'a, Message> {
        let mut choices = vec![FolderChoice(None, "No folder".into())];
        choices.extend(self.sidebar.folders.iter().map(|f| FolderChoice(Some(f.id), f.name.clone())));
        let selected = choices.iter().find(|c| c.0 == edit.folder).cloned();
        let url = self.sidebar.feeds.iter().find(|f| f.id == edit.id).map(|f| f.url.as_str()).unwrap_or_default();
        container(
            column![
                text(url).size(11),
                row![
                    text_input("Title", &edit.title).on_input(Message::FeedTitle).on_submit(Message::SaveFeed),
                    pick_list(choices, selected, Message::FeedFolder),
                    text_input("or new folder", &edit.new_folder).on_input(Message::NewFolder).width(140),
                ]
                .spacing(6),
                row![
                    button(text("Save").size(12)).on_press(Message::SaveFeed),
                    button(text("Cancel").size(12)).style(button::secondary).on_press(Message::CancelEditFeed),
                ]
                .spacing(6),
            ]
            .spacing(6),
        )
        .padding([6, 12])
        .into()
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

    fn view_preview(&self) -> Element<'_, Message> {
        let Some(p) = &self.preview else {
            return container(
                text("Select an article. Keys: j/k move, s save, m read/unread, o open, / search.").size(14),
            )
            .center(Fill)
            .into();
        };
        let a = &p.article;
        let mut meta = row![text(&p.feed_title).size(13)].spacing(12);
        meta = meta.push(text(format::full_date(a.effective_at)).size(13));
        if a.published_at.is_none() {
            meta = meta.push(text("(date fetched; feed gave no date)").size(12));
        }
        if let Some(author) = &a.author {
            meta = meta.push(text(author).size(13));
        }
        let actions = row![
            button(text(if a.is_saved { "★ Saved" } else { "☆ Save" }))
                .style(button::secondary)
                .on_press(Message::ToggleSaved),
            button(text(if a.is_read { "Mark unread" } else { "Mark read" }))
                .style(button::secondary)
                .on_press(Message::ToggleRead),
            button(text("Open in browser")).on_press_maybe(a.url.as_ref().map(|_| Message::OpenInBrowser)),
            button(text("Map")).style(button::secondary).on_press(Message::MapArticle),
        ]
        .spacing(8);
        let mut labels = row![].spacing(6);
        for l in &p.labels {
            let marker = match l.status {
                juttusihti_core::Status::Confirmed => "",
                juttusihti_core::Status::Suggested => " (AI suggestion)",
                juttusihti_core::Status::Rejected => continue,
            };
            let id = l.label.id;
            let mut chip = row![text(format!("{}{marker}", l.label.name)).size(12)].spacing(4).align_y(iced::Center);
            if l.status == juttusihti_core::Status::Suggested {
                chip = chip
                    .push(
                        button(text("✓").size(11))
                            .style(button::text)
                            .padding(0)
                            .on_press(Message::DecideLabel(id, juttusihti_core::Status::Confirmed)),
                    )
                    .push(
                        button(text("✕").size(11))
                            .style(button::text)
                            .padding(0)
                            .on_press(Message::DecideLabel(id, juttusihti_core::Status::Rejected)),
                    );
            } else {
                chip = chip
                    .push(button(text("×").size(11)).style(button::text).padding(0).on_press(Message::RemoveLabel(id)));
            }
            labels = labels.push(container(chip).padding([2, 8]).style(container::rounded_box));
        }
        labels = labels.push(
            text_input("+ label", &self.label_input)
                .on_input(Message::LabelInput)
                .on_submit(Message::AddLabel)
                .size(12)
                .width(120),
        );
        let body = if p.body.is_empty() {
            "This entry has no text in the feed. Open it in the browser to read it."
        } else {
            &p.body
        };
        scrollable(
            column![
                text(&a.title).size(24).font(Font { weight: font::Weight::Bold, ..Font::DEFAULT }),
                meta,
                actions,
                labels,
                rule::horizontal(1),
                text(body).size(15).line_height(1.5),
                Space::new().height(24),
            ]
            .spacing(12)
            .padding(24)
            .max_width(820),
        )
        .height(Fill)
        .into()
    }
}

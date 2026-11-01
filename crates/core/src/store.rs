//! Reader storage: folders, feeds, articles, read state, bookmarks,
//! retention, settings and saved searches.

use rusqlite::{Connection, OptionalExtension, params};

use crate::normalize::{content_hash, dedup_key, html_to_text};
use crate::search::{self, SearchQuery, SearchResults};
use crate::{
    Article, ArticleId, ArticleSummary, Error, Feed, FeedId, Folder, FolderId, IngestStats, NewArticle, NewFeed, Page,
    Result, View, now,
};

/// Owns the read-write SQLite connection. Not `Sync`; the UI drives it from
/// one worker thread.
pub struct Store {
    conn: Connection,
}

/// Which articles a list shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ListFilter {
    pub view: View,
    pub feed_id: Option<FeedId>,
    pub folder_id: Option<FolderId>,
}

impl Store {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Ok(Self { conn: crate::db::open(path)? })
    }

    pub fn open_in_memory() -> Result<Self> {
        Ok(Self { conn: crate::db::open_in_memory()? })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    // ---- folders -------------------------------------------------------

    pub fn create_folder(&self, name: &str) -> Result<FolderId> {
        let name = non_empty(name, "folder name")?;
        self.conn.execute(
            "INSERT INTO folders(name, created_at) VALUES (?1, ?2) ON CONFLICT(name) DO NOTHING",
            params![name, now()],
        )?;
        Ok(self.conn.query_row("SELECT id FROM folders WHERE name = ?1", [name], |r| r.get(0))?)
    }

    pub fn folders(&self) -> Result<Vec<Folder>> {
        let mut stmt = self.conn.prepare("SELECT id, name FROM folders ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| Ok(Folder { id: r.get(0)?, name: r.get(1)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn rename_folder(&self, id: FolderId, name: &str) -> Result<()> {
        let name = non_empty(name, "folder name")?;
        expect_one(self.conn.execute("UPDATE folders SET name = ?1 WHERE id = ?2", params![name, id])?)
    }

    /// Deletes a folder; its feeds become unfiled.
    pub fn delete_folder(&self, id: FolderId) -> Result<()> {
        expect_one(self.conn.execute("DELETE FROM folders WHERE id = ?1", [id])?)
    }

    // ---- feeds ---------------------------------------------------------

    /// Adds a subscription. Returns the existing id if the URL is already subscribed.
    pub fn add_feed(&self, feed: &NewFeed) -> Result<FeedId> {
        let url = non_empty(&feed.url, "feed URL")?;
        if !crate::feed::is_web_url(url) {
            return Err(Error::Invalid(format!("not an http(s) URL: {url}")));
        }
        let title = if feed.title.trim().is_empty() { url } else { feed.title.trim() };
        self.conn.execute(
            "INSERT INTO feeds(url, title, site_url, folder_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(url) DO NOTHING",
            params![url, title, feed.site_url, feed.folder_id, now()],
        )?;
        Ok(self.conn.query_row("SELECT id FROM feeds WHERE url = ?1", [url], |r| r.get(0))?)
    }

    pub fn feed(&self, id: FeedId) -> Result<Feed> {
        self.conn
            .query_row(&format!("SELECT {FEED_COLS} FROM feeds WHERE id = ?1"), [id], feed_row)
            .optional()?
            .ok_or(Error::NotFound)
    }

    pub fn feeds(&self) -> Result<Vec<Feed>> {
        let mut stmt = self.conn.prepare(&format!("SELECT {FEED_COLS} FROM feeds ORDER BY title COLLATE NOCASE"))?;
        let rows = stmt.query_map([], feed_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Unread counts per feed.
    pub fn unread_counts(&self) -> Result<Vec<(FeedId, u64)>> {
        let mut stmt =
            self.conn.prepare("SELECT feed_id, count(*) FROM articles WHERE is_read = 0 GROUP BY feed_id")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get::<_, i64>(1)? as u64)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn update_feed(&self, id: FeedId, title: &str, folder_id: Option<FolderId>) -> Result<()> {
        let title = non_empty(title, "feed title")?;
        expect_one(
            self.conn
                .execute("UPDATE feeds SET title = ?1, folder_id = ?2 WHERE id = ?3", params![title, folder_id, id])?,
        )
    }

    /// Turns full-article fetching on or off for a feed.
    pub fn set_feed_full_text(&self, id: FeedId, on: bool) -> Result<()> {
        expect_one(self.conn.execute("UPDATE feeds SET fetch_full_text = ?1 WHERE id = ?2", params![on, id])?)
    }

    /// Removes a subscription and its articles (and their research records).
    pub fn remove_feed(&self, id: FeedId) -> Result<()> {
        expect_one(self.conn.execute("DELETE FROM feeds WHERE id = ?1", [id])?)
    }

    /// Records a successful fetch and the HTTP cache validators.
    pub fn record_fetch_ok(&self, id: FeedId, etag: Option<&str>, last_modified: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE feeds SET etag = ?1, last_modified = ?2, last_fetched_at = ?3, last_error = NULL WHERE id = ?4",
            params![etag, last_modified, now(), id],
        )?;
        Ok(())
    }

    /// Records a 304 Not Modified response.
    pub fn record_not_modified(&self, id: FeedId) -> Result<()> {
        self.conn
            .execute("UPDATE feeds SET last_fetched_at = ?1, last_error = NULL WHERE id = ?2", params![now(), id])?;
        Ok(())
    }

    pub fn record_fetch_error(&self, id: FeedId, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE feeds SET last_fetched_at = ?1, last_error = ?2 WHERE id = ?3",
            params![now(), error, id],
        )?;
        Ok(())
    }

    /// Fills in a feed's title and site URL from the document if the user has
    /// not set a title (it still equals the URL).
    pub fn adopt_feed_metadata(&self, id: FeedId, title: Option<&str>, site_url: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE feeds SET title = CASE WHEN title = url AND ?1 IS NOT NULL THEN ?1 ELSE title END,
                              site_url = COALESCE(site_url, ?2)
             WHERE id = ?3",
            params![title, site_url, id],
        )?;
        Ok(())
    }

    // ---- articles ------------------------------------------------------

    /// Stores parsed entries for a feed in one transaction. Existing entries
    /// (same feed + guid) are updated only if the source content changed;
    /// read/saved state is preserved.
    pub fn ingest(&mut self, feed_id: FeedId, entries: &[NewArticle]) -> Result<IngestStats> {
        let fetched_at = now();
        let tx = self.conn.transaction()?;
        let mut stats = IngestStats::default();
        {
            let mut existing =
                tx.prepare_cached("SELECT id, content_hash FROM articles WHERE feed_id = ?1 AND guid = ?2")?;
            let mut same_story = tx.prepare_cached("SELECT 1 FROM articles WHERE feed_id = ?1 AND dedup_key = ?2")?;
            let mut insert = tx.prepare_cached(
                "INSERT INTO articles(feed_id, guid, url, title, summary, content, summary_text, content_text, author,
                                      published_at, fetched_at, effective_at, dedup_key, content_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            )?;
            let mut update = tx.prepare_cached(
                "UPDATE articles SET url = ?2, title = ?3, summary = ?4, content = ?5, summary_text = ?6,
                                     content_text = ?7, author = ?8, published_at = COALESCE(?9, published_at),
                                     effective_at = COALESCE(?9, effective_at), dedup_key = ?10, content_hash = ?11
                 WHERE id = ?1",
            )?;
            for e in entries {
                let hash = content_hash(&e.title, e.summary.as_deref(), e.content.as_deref());
                let key = dedup_key(e.url.as_deref(), &e.title);
                let summary_text = e.summary.as_deref().map(html_to_text);
                let content_text = e.content.as_deref().map(html_to_text);
                let found: Option<(i64, String)> =
                    existing.query_row(params![feed_id, e.guid], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
                match found {
                    Some((_, old)) if old == hash => stats.unchanged += 1,
                    Some((id, _)) => {
                        update.execute(params![
                            id,
                            e.url,
                            e.title,
                            e.summary,
                            e.content,
                            summary_text,
                            content_text,
                            e.author,
                            e.published_at,
                            key,
                            hash
                        ])?;
                        stats.updated += 1;
                    }
                    // Same URL re-published by the same feed under a new guid.
                    // Title-only keys are not trusted for this ("Päivän sää" recurs daily).
                    None if key.starts_with("url:") && same_story.exists(params![feed_id, key])? => {
                        stats.unchanged += 1
                    }
                    None => {
                        insert.execute(params![
                            feed_id,
                            e.guid,
                            e.url,
                            e.title,
                            e.summary,
                            e.content,
                            summary_text,
                            content_text,
                            e.author,
                            e.published_at,
                            fetched_at,
                            e.published_at.unwrap_or(fetched_at),
                            key,
                            hash
                        ])?;
                        stats.inserted += 1;
                    }
                }
            }
        }
        tx.commit()?;
        Ok(stats)
    }

    pub fn article(&self, id: ArticleId) -> Result<Article> {
        self.conn
            .query_row(
                "SELECT id, feed_id, guid, url, title, summary, content, author, published_at, fetched_at,
                        effective_at, dedup_key, is_read, is_saved
                 FROM articles WHERE id = ?1",
                [id],
                |r| {
                    Ok(Article {
                        id: r.get(0)?,
                        feed_id: r.get(1)?,
                        guid: r.get(2)?,
                        url: r.get(3)?,
                        title: r.get(4)?,
                        summary: r.get(5)?,
                        content: r.get(6)?,
                        author: r.get(7)?,
                        published_at: r.get(8)?,
                        fetched_at: r.get(9)?,
                        effective_at: r.get(10)?,
                        dedup_key: r.get(11)?,
                        is_read: r.get(12)?,
                        is_saved: r.get(13)?,
                    })
                },
            )
            .optional()?
            .ok_or(Error::NotFound)
    }

    /// Plain-text body for display: text extracted from the article page if
    /// it was fetched, else the feed's content, else its summary.
    pub fn article_text(&self, id: ArticleId) -> Result<String> {
        self.conn
            .query_row(
                "SELECT COALESCE(NULLIF(e.text, ''), NULLIF(a.content_text, ''), a.summary_text, '')
                 FROM articles a LEFT JOIN article_extracts e ON e.article_id = a.id WHERE a.id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(Error::NotFound)
    }

    /// Paginated article list, newest first.
    pub fn list_articles(&self, filter: ListFilter, page: Page) -> Result<(Vec<ArticleSummary>, u64)> {
        let q = SearchQuery {
            view: filter.view,
            feed_ids: filter.feed_id.into_iter().collect(),
            folder_ids: filter.folder_id.into_iter().collect(),
            ..Default::default()
        };
        let r = search::search(&self.conn, &q, page)?;
        Ok((r.items, r.total))
    }

    pub fn search(&self, q: &SearchQuery, page: Page) -> Result<SearchResults> {
        search::search(&self.conn, q, page)
    }

    pub fn set_read(&self, id: ArticleId, read: bool) -> Result<()> {
        expect_one(self.conn.execute("UPDATE articles SET is_read = ?1 WHERE id = ?2", params![read, id])?)
    }

    pub fn set_saved(&self, id: ArticleId, saved: bool) -> Result<()> {
        expect_one(self.conn.execute("UPDATE articles SET is_saved = ?1 WHERE id = ?2", params![saved, id])?)
    }

    /// Marks everything in a list as read. Returns the number changed.
    pub fn mark_all_read(&self, filter: ListFilter) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE articles SET is_read = 1
             WHERE is_read = 0
               AND (?1 IS NULL OR feed_id = ?1)
               AND (?2 IS NULL OR feed_id IN (SELECT id FROM feeds WHERE folder_id = ?2))
               AND (?3 = 0 OR is_saved = 1)",
            params![filter.feed_id, filter.folder_id, filter.view == View::Saved],
        )?)
    }

    /// Deletes articles whose effective date is older than `days`, keeping
    /// bookmarked articles and anything the user annotated or confirmed in
    /// research. Returns the number deleted.
    pub fn apply_retention(&self, days: u32) -> Result<usize> {
        let cutoff = now() - i64::from(days) * 86_400;
        Ok(self.conn.execute(
            "DELETE FROM articles
             WHERE effective_at < ?1 AND is_saved = 0
               AND NOT EXISTS (SELECT 1 FROM annotations n WHERE n.article_id = articles.id)
               AND NOT EXISTS (SELECT 1 FROM article_labels l WHERE l.article_id = articles.id AND l.status = 'confirmed')
               AND NOT EXISTS (SELECT 1 FROM article_relationships r
                               WHERE (r.source_id = articles.id OR r.target_id = articles.id) AND r.status = 'confirmed')
               AND NOT EXISTS (SELECT 1 FROM analysis_session_articles s WHERE s.article_id = articles.id)",
            [cutoff],
        )?)
    }

    /// Other stored articles sharing this article's dedup key (syndicated or
    /// repeated copies).
    pub fn duplicates_of(&self, id: ArticleId) -> Result<Vec<ArticleId>> {
        let mut stmt = self.conn.prepare(
            "SELECT b.id FROM articles a JOIN articles b ON b.dedup_key = a.dedup_key AND b.id <> a.id
             WHERE a.id = ?1 ORDER BY b.effective_at",
        )?;
        let rows = stmt.query_map([id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- settings and saved searches -----------------------------------

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn save_search(&self, name: &str, q: &SearchQuery) -> Result<i64> {
        let name = non_empty(name, "search name")?;
        // Validate before storing so saved searches always run.
        search::parse_query(&q.text)?;
        self.conn.execute(
            "INSERT INTO saved_searches(name, query_json, created_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET query_json = excluded.query_json",
            params![name, serde_json::to_string(q)?, now()],
        )?;
        Ok(self.conn.query_row("SELECT id FROM saved_searches WHERE name = ?1", [name], |r| r.get(0))?)
    }

    pub fn saved_searches(&self) -> Result<Vec<(i64, String, SearchQuery)>> {
        let mut stmt = self.conn.prepare("SELECT id, name, query_json FROM saved_searches ORDER BY name")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?;
        rows.map(|row| {
            let (id, name, json) = row?;
            Ok((id, name, serde_json::from_str(&json)?))
        })
        .collect()
    }

    pub fn delete_saved_search(&self, id: i64) -> Result<()> {
        expect_one(self.conn.execute("DELETE FROM saved_searches WHERE id = ?1", [id])?)
    }
}

const FEED_COLS: &str =
    "id, url, title, site_url, folder_id, etag, last_modified, last_fetched_at, last_error, fetch_full_text";

fn feed_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Feed> {
    Ok(Feed {
        id: r.get(0)?,
        url: r.get(1)?,
        title: r.get(2)?,
        site_url: r.get(3)?,
        folder_id: r.get(4)?,
        etag: r.get(5)?,
        last_modified: r.get(6)?,
        last_fetched_at: r.get(7)?,
        last_error: r.get(8)?,
        fetch_full_text: r.get(9)?,
    })
}

fn non_empty<'a>(s: &'a str, what: &str) -> Result<&'a str> {
    let s = s.trim();
    if s.is_empty() { Err(Error::Invalid(format!("{what} must not be empty"))) } else { Ok(s) }
}

fn expect_one(changed: usize) -> Result<()> {
    if changed == 0 { Err(Error::NotFound) } else { Ok(()) }
}

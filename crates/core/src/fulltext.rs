//! Full article text fetched from the web, stored apart from the feed's own
//! content. Feed refreshes never touch it; it is searchable through the
//! `fetched_text` FTS column and feeds the "Archived article text" depth.
//!
//! The table is a cache bounded by a byte limit: eviction removes the oldest
//! fetches of articles that are not bookmarked. Picture files live in the
//! data directory; these functions return their relative paths so the caller
//! can delete them.

use rusqlite::{OptionalExtension, params};

use crate::store::Store;
use crate::{ArticleId, Error, Result, now};

/// Fetched text for one article.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fulltext {
    pub article_id: ArticleId,
    pub text: String,
    pub paywalled: bool,
    /// Relative to the data directory.
    pub picture_path: Option<String>,
    pub fetched_at: i64,
}

impl Store {
    /// Stores (or replaces) the fetched text of an article. `picture` is the
    /// picture's path relative to the data directory and its size in bytes.
    pub fn save_fulltext(
        &self,
        id: ArticleId,
        text: &str,
        paywalled: bool,
        picture: Option<(&str, u64)>,
    ) -> Result<()> {
        let exists = self.conn().query_row("SELECT 1 FROM articles WHERE id = ?1", [id], |_| Ok(())).optional()?;
        if exists.is_none() {
            return Err(Error::NotFound);
        }
        let (picture_path, picture_bytes) = match picture {
            Some((path, bytes)) => (Some(path), bytes as i64),
            None => (None, 0),
        };
        self.conn().execute(
            "INSERT INTO article_fulltext(article_id, text, paywalled, picture_path, text_bytes, picture_bytes, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(article_id) DO UPDATE SET
                 text = excluded.text, paywalled = excluded.paywalled, picture_path = excluded.picture_path,
                 text_bytes = excluded.text_bytes, picture_bytes = excluded.picture_bytes,
                 fetched_at = excluded.fetched_at",
            params![id, text, paywalled, picture_path, text.len() as i64, picture_bytes, now()],
        )?;
        Ok(())
    }

    pub fn fulltext(&self, id: ArticleId) -> Result<Option<Fulltext>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT article_id, text, paywalled, picture_path, fetched_at FROM article_fulltext WHERE article_id = ?1",
                [id],
                |r| {
                    Ok(Fulltext {
                        article_id: r.get(0)?,
                        text: r.get(1)?,
                        paywalled: r.get(2)?,
                        picture_path: r.get(3)?,
                        fetched_at: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// Bytes the cache holds: text plus pictures.
    pub fn fulltext_cache_bytes(&self) -> Result<u64> {
        let n: i64 = self.conn().query_row(
            "SELECT COALESCE(SUM(text_bytes + picture_bytes), 0) FROM article_fulltext",
            [],
            |r| r.get(0),
        )?;
        Ok(n.max(0) as u64)
    }

    /// Evicts the oldest fetches of unbookmarked articles until the cache
    /// fits in `limit_bytes`. Bookmarked articles are kept even if that
    /// leaves the cache over the limit. Returns the removed picture paths.
    pub fn enforce_fulltext_limit(&self, limit_bytes: u64) -> Result<Vec<String>> {
        let tx = self.conn().unchecked_transaction()?;
        let mut total = self.fulltext_cache_bytes()?;
        let mut removed = Vec::new();
        if total > limit_bytes {
            // Read the candidates first; deleting while scanning the same
            // table is not well defined in SQLite.
            let candidates: Vec<(ArticleId, Option<String>, i64)> = tx
                .prepare(
                    "SELECT ft.article_id, ft.picture_path, ft.text_bytes + ft.picture_bytes
                     FROM article_fulltext ft JOIN articles a ON a.id = ft.article_id
                     WHERE a.is_saved = 0
                     ORDER BY ft.fetched_at, ft.article_id",
                )?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut delete = tx.prepare("DELETE FROM article_fulltext WHERE article_id = ?1")?;
            for (id, path, bytes) in candidates {
                if total <= limit_bytes {
                    break;
                }
                delete.execute([id])?;
                total = total.saturating_sub(bytes.max(0) as u64);
                removed.extend(path);
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    /// Removes all fetched text, bookmarked articles included. Returns the
    /// removed picture paths.
    pub fn clear_fulltext(&self) -> Result<Vec<String>> {
        let tx = self.conn().unchecked_transaction()?;
        let paths = self.fulltext_picture_paths()?;
        tx.execute("DELETE FROM article_fulltext", [])?;
        tx.commit()?;
        Ok(paths)
    }

    /// Every picture path the cache refers to, for sweeping orphaned files.
    pub fn fulltext_picture_paths(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn()
            .prepare("SELECT picture_path FROM article_fulltext WHERE picture_path IS NOT NULL ORDER BY article_id")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

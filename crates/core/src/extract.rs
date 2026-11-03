//! Full-article text for feeds that only carry teasers.
//!
//! Feeds opt in one by one ([`Store::set_feed_full_text`]). For their
//! entries whose feed text is short, the article page is fetched (by
//! `juttusihti-fetch`) and its main text is extracted here with
//! [`dom_smoothie`], a port of Mozilla's Readability. The text is stored in
//! `article_extracts`, apart from the feed's own content, and is preferred
//! for reading, search and AI input when present.
//!
//! Article pages are untrusted: their size is capped before parsing, the
//! number of elements the extractor walks is bounded, scripts never run and
//! only plain text is kept.

use rusqlite::{OptionalExtension, params};

use crate::normalize::{html_to_text, sha256_hex};
use crate::store::Store;
use crate::{ArticleId, Error, FeedId, Result, now};

/// Entries whose feed content and summary are both shorter than this (in
/// characters of plain text) count as teasers.
pub const TEASER_MAX_CHARS: usize = 600;
/// Largest article page that is read and parsed.
pub const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;
/// Entries older than this are not fetched when a feed is switched on, so
/// turning the option on does not crawl a long backlog.
pub const MAX_AGE_DAYS: i64 = 14;
/// Most article pages fetched per feed per refresh.
pub const MAX_PER_REFRESH: u32 = 20;
/// Extracted text shorter than this is treated as a failed extraction
/// (cookie walls, paywalls, link lists).
pub const MIN_EXTRACT_CHARS: usize = 200;
/// Bound on the elements the extractor scores, against pathological pages.
const MAX_ELEMENTS: usize = 50_000;

/// An entry whose article page should be fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub article_id: ArticleId,
    pub url: String,
}

/// What came of fetching one article page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extraction {
    /// Main text extracted from the page.
    Text(String),
    /// The page was read but held no usable article text.
    NoText,
    /// The page could not be fetched.
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractStatus {
    /// Text was extracted and is longer than the feed's own text.
    Ok,
    /// Nothing better than the feed text was found.
    Empty,
    Failed,
}

impl ExtractStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Empty => "empty",
            Self::Failed => "failed",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "ok" => Self::Ok,
            "empty" => Self::Empty,
            _ => Self::Failed,
        }
    }
}

/// The stored result for one article.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticleExtract {
    /// The page that was read, after redirects.
    pub url: String,
    pub status: ExtractStatus,
    pub text: Option<String>,
    pub error: Option<String>,
    pub fetched_at: i64,
}

/// Extracts the main article text from an HTML page. `page_url` resolves
/// relative links. Returns `None` when the page is too large, cannot be
/// parsed or yields less than [`MIN_EXTRACT_CHARS`] of text.
pub fn extract_main_text(html: &str, page_url: &str) -> Option<String> {
    if html.len() > MAX_PAGE_BYTES {
        return None;
    }
    let cfg = dom_smoothie::Config { max_elements_to_parse: MAX_ELEMENTS, ..Default::default() };
    let mut readability = dom_smoothie::Readability::new(html, Some(page_url), Some(cfg)).ok()?;
    let article = readability.parse().ok()?;
    // The cleaned article HTML goes through the same reduction as feed
    // content, so paragraphs and entities are handled identically.
    let text = crate::feed::bounded(html_to_text(&article.content));
    (text.chars().count() >= MIN_EXTRACT_CHARS).then_some(text)
}

impl Store {
    /// Teaser-only entries of a feed with full-article fetching on that have
    /// not been tried yet, newest first.
    pub fn full_text_candidates(&self, feed_id: FeedId, limit: u32) -> Result<Vec<Candidate>> {
        let mut stmt = self.conn().prepare_cached(
            "SELECT a.id, a.url FROM articles a JOIN feeds f ON f.id = a.feed_id
             WHERE a.feed_id = ?1 AND f.fetch_full_text = 1 AND a.url IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM article_extracts e WHERE e.article_id = a.id)
               AND length(COALESCE(a.content_text, '')) < ?2
               AND length(COALESCE(a.summary_text, '')) < ?2
               AND a.effective_at >= ?3
             ORDER BY a.effective_at DESC, a.id DESC LIMIT ?4",
        )?;
        let since = now() - MAX_AGE_DAYS * 86_400;
        let rows = stmt.query_map(params![feed_id, TEASER_MAX_CHARS as i64, since, limit], |r| {
            Ok(Candidate { article_id: r.get(0)?, url: r.get(1)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// The page to fetch for one article on request, whatever its feed's
    /// setting, length or age. Fails with [`Error::Invalid`] when the entry
    /// has no web link.
    pub fn full_text_target(&self, article_id: ArticleId) -> Result<Candidate> {
        let url: Option<String> = self
            .conn()
            .query_row("SELECT url FROM articles WHERE id = ?1", [article_id], |r| r.get(0))
            .optional()?
            .ok_or(Error::NotFound)?;
        match url {
            Some(url) if crate::feed::is_web_url(&url) => Ok(Candidate { article_id, url }),
            _ => Err(Error::Invalid("this entry has no link to an article page".into())),
        }
    }

    /// Records the result of fetching an article page and returns how it
    /// went. Text no longer than the feed's own text counts as
    /// [`ExtractStatus::Empty`]. A failed or empty attempt does not replace
    /// text stored earlier. An article deleted in the meantime is ignored.
    pub fn save_extract(&self, article_id: ArticleId, url: &str, result: Extraction) -> Result<ExtractStatus> {
        let feed_len: Option<i64> = self
            .conn()
            .query_row(
                "SELECT max(length(COALESCE(content_text, '')), length(COALESCE(summary_text, '')))
                 FROM articles WHERE id = ?1",
                [article_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(feed_len) = feed_len else { return Ok(ExtractStatus::Failed) };
        let (status, text, error) = match result {
            Extraction::Text(t) if t.chars().count() as i64 > feed_len => (ExtractStatus::Ok, Some(t), None),
            Extraction::Text(_) | Extraction::NoText => (ExtractStatus::Empty, None, None),
            Extraction::Failed(e) => (ExtractStatus::Failed, None, Some(e)),
        };
        // A failed or empty re-fetch never replaces text fetched earlier.
        if status != ExtractStatus::Ok
            && self.article_extract(article_id)?.is_some_and(|e| e.status == ExtractStatus::Ok)
        {
            return Ok(status);
        }
        let hash = text.as_deref().map(sha256_hex);
        self.conn().execute(
            "INSERT INTO article_extracts(article_id, url, status, text, text_hash, error, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(article_id) DO UPDATE SET url = excluded.url, status = excluded.status,
                 text = excluded.text, text_hash = excluded.text_hash, error = excluded.error,
                 fetched_at = excluded.fetched_at",
            params![article_id, url, status.as_str(), text, hash, error, now()],
        )?;
        Ok(status)
    }

    /// The stored full-article result for an article, if it was tried.
    pub fn article_extract(&self, article_id: ArticleId) -> Result<Option<ArticleExtract>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT url, status, text, error, fetched_at FROM article_extracts WHERE article_id = ?1",
                [article_id],
                |r| {
                    Ok(ArticleExtract {
                        url: r.get(0)?,
                        status: ExtractStatus::parse(&r.get::<_, String>(1)?),
                        text: r.get(2)?,
                        error: r.get(3)?,
                        fetched_at: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// Forgets the full-article result for an article, so the next refresh
    /// tries its page again.
    pub fn clear_extract(&self, article_id: ArticleId) -> Result<()> {
        self.conn().execute("DELETE FROM article_extracts WHERE article_id = ?1", [article_id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARTICLE: &str = include_str!("../tests/fixtures/article_page.html");

    #[test]
    fn extracts_the_article_body_and_drops_chrome() {
        let text = extract_main_text(ARTICLE, "https://news.example/2026/11/satama").unwrap();
        assert!(text.contains("Helsingin sataman laajennus"), "{text}");
        assert!(text.contains("vuoteen 2028"), "{text}");
        assert!(!text.contains("Tilaa uutiskirje"), "navigation and footer must be dropped: {text}");
        assert!(!text.contains("alert("), "scripts must never reach the text: {text}");
        assert!(text.contains('\n'), "paragraphs stay separate");
    }

    #[test]
    fn rejects_pages_without_an_article() {
        let html = "<html><body><nav><a href='/'>Etusivu</a></nav><p>Hyväksy evästeet.</p></body></html>";
        assert_eq!(extract_main_text(html, "https://news.example/"), None);
        assert_eq!(extract_main_text("", "https://news.example/"), None);
    }

    #[test]
    fn rejects_oversized_pages_before_parsing() {
        let mut big = String::from("<html><body><article><p>");
        big.push_str(&"sana ".repeat(MAX_PAGE_BYTES / 5 + 1));
        big.push_str("</p></article></body></html>");
        assert_eq!(extract_main_text(&big, "https://news.example/"), None);
    }
}

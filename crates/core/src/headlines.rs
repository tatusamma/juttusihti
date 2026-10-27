//! AI-rewritten headlines. A rewrite is shown instead of a clickbait or
//! vague title until the user rejects it; the article's own title is never
//! changed. A user's decision is never overridden by a later AI run.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params, params_from_iter};

use crate::store::Store;
use crate::{ArticleId, Result, Status, now};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Headline {
    pub article_id: ArticleId,
    pub headline: String,
    pub status: Status,
}

impl Store {
    /// Stores an AI rewrite. A rewrite the user already kept or rejected is
    /// left as it is.
    pub fn suggest_headline(
        &self,
        article: ArticleId,
        headline: &str,
        session: Option<i64>,
        suggestion: Option<i64>,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO article_headlines(article_id, headline, status, session_id, suggestion_id, created_at)
             VALUES (?1, ?2, 'suggested', ?3, ?4, ?5)
             ON CONFLICT(article_id) DO UPDATE SET
               headline = excluded.headline, session_id = excluded.session_id,
               suggestion_id = excluded.suggestion_id, created_at = excluded.created_at
             WHERE article_headlines.status = 'suggested'",
            params![article, headline, session, suggestion, now()],
        )?;
        Ok(())
    }

    pub fn headline(&self, article: ArticleId) -> Result<Option<Headline>> {
        Ok(self
            .conn()
            .query_row(
                "SELECT article_id, headline, status FROM article_headlines WHERE article_id = ?1",
                [article],
                |r| Ok(Headline { article_id: r.get(0)?, headline: r.get(1)?, status: r.get(2)? }),
            )
            .optional()?)
    }

    /// Rewrites to show for a page of articles (not rejected ones).
    pub fn shown_headlines(&self, articles: &[ArticleId]) -> Result<HashMap<ArticleId, String>> {
        if articles.is_empty() {
            return Ok(HashMap::new());
        }
        let marks = vec!["?"; articles.len()].join(",");
        let mut stmt = self.conn().prepare(&format!(
            "SELECT article_id, headline FROM article_headlines
             WHERE status <> 'rejected' AND article_id IN ({marks})"
        ))?;
        let rows = stmt.query_map(params_from_iter(articles), |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn decide_headline(&self, article: ArticleId, status: Status) -> Result<()> {
        self.conn().execute(
            "UPDATE article_headlines SET status = ?2, decided_at = ?3 WHERE article_id = ?1",
            params![article, status, now()],
        )?;
        Ok(())
    }
}

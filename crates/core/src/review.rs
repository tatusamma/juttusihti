//! Review queues for AI suggestions produced by an analysis session:
//! entity mentions, relationships, relevance classifications and the
//! research summary. Users confirm or reject; nothing is deleted.

use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::store::Store;
use crate::{ArticleId, EntityKind, Error, Origin, RelationshipKind, Result, Status, now};

/// Relevance classes that the user can choose to hide from article lists
/// once confirmed. Hiding is a filter, never a deletion.
pub const DISMISSED_CLASSES: &str = "('unrelated', 'duplicate', 'low_information', 'clickbait')";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionRow {
    pub mention_id: i64,
    pub article_id: ArticleId,
    pub article_title: String,
    pub entity_id: i64,
    pub entity_name: String,
    pub kind: EntityKind,
    pub role: String,
    pub evidence: Option<String>,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RelationshipRow {
    pub id: i64,
    pub source_id: ArticleId,
    pub source_title: String,
    pub target_id: ArticleId,
    pub target_title: String,
    pub kind: RelationshipKind,
    pub evidence: Option<String>,
    pub confidence: Option<f64>,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelevanceRow {
    pub id: i64,
    pub article_id: ArticleId,
    pub article_title: String,
    pub class: String,
    pub reason: Option<String>,
    pub status: Status,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReviewCounts {
    pub entities: u64,
    pub relationships: u64,
    pub relevance: u64,
    pub has_summary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ReviewKind {
    Entities,
    Relationships,
    Relevance,
}

const RELEVANCE_CLASSES: [&str; 6] =
    ["relevant", "possibly_relevant", "unrelated", "duplicate", "low_information", "clickbait"];

fn status_filter(only_pending: bool) -> &'static str {
    if only_pending { "= 'suggested'" } else { "<> 'rejected'" }
}

impl Store {
    /// Pending (suggested) counts for a session's review queues.
    pub fn review_counts(&self, session: i64) -> Result<ReviewCounts> {
        let c = self.conn();
        let count = |sql: &str| -> Result<u64> { Ok(c.query_row(sql, [session], |r| r.get::<_, i64>(0))? as u64) };
        Ok(ReviewCounts {
            entities: count(
                "SELECT count(*) FROM article_entities ae JOIN ai_suggestions s ON s.id = ae.suggestion_id
                 WHERE s.session_id = ?1 AND ae.status = 'suggested'",
            )?,
            relationships: count(
                "SELECT count(*) FROM article_relationships r JOIN ai_suggestions s ON s.id = r.suggestion_id
                 WHERE s.session_id = ?1 AND r.status = 'suggested'",
            )?,
            relevance: count("SELECT count(*) FROM article_relevance WHERE session_id = ?1 AND status = 'suggested'")?,
            has_summary: count("SELECT count(*) FROM ai_suggestions WHERE session_id = ?1 AND kind = 'summary'")? > 0,
        })
    }

    pub fn session_mentions(&self, session: i64, only_pending: bool, limit: u32) -> Result<Vec<MentionRow>> {
        let sql = format!(
            "SELECT ae.id, a.id, a.title, e.id, e.name, e.kind, ae.role, ae.evidence, ae.status
             FROM article_entities ae
             JOIN ai_suggestions s ON s.id = ae.suggestion_id
             JOIN articles a ON a.id = ae.article_id
             JOIN entities e ON e.id = ae.entity_id
             WHERE s.session_id = ?1 AND ae.status {}
             ORDER BY e.name COLLATE NOCASE, a.effective_at DESC LIMIT ?2",
            status_filter(only_pending)
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![session, limit], |r| {
            Ok(MentionRow {
                mention_id: r.get(0)?,
                article_id: r.get(1)?,
                article_title: r.get(2)?,
                entity_id: r.get(3)?,
                entity_name: r.get(4)?,
                kind: r.get(5)?,
                role: r.get(6)?,
                evidence: r.get(7)?,
                status: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn decide_mention(&self, mention_id: i64, status: Status) -> Result<()> {
        let n = self
            .conn()
            .execute("UPDATE article_entities SET status = ?2 WHERE id = ?1", params![mention_id, status])?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    pub fn session_relationships(&self, session: i64, only_pending: bool, limit: u32) -> Result<Vec<RelationshipRow>> {
        let sql = format!(
            "SELECT r.id, a.id, a.title, b.id, b.title, r.kind, r.evidence, r.confidence, r.status
             FROM article_relationships r
             JOIN ai_suggestions s ON s.id = r.suggestion_id
             JOIN articles a ON a.id = r.source_id
             JOIN articles b ON b.id = r.target_id
             WHERE s.session_id = ?1 AND r.status {}
             ORDER BY r.confidence DESC, r.id LIMIT ?2",
            status_filter(only_pending)
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![session, limit], |r| {
            Ok(RelationshipRow {
                id: r.get(0)?,
                source_id: r.get(1)?,
                source_title: r.get(2)?,
                target_id: r.get(3)?,
                target_title: r.get(4)?,
                kind: r.get(5)?,
                evidence: r.get(6)?,
                confidence: r.get(7)?,
                status: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Changes a relationship's type; the user's choice makes it confirmed.
    pub fn retype_relationship(&self, id: i64, kind: RelationshipKind) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE article_relationships SET kind = ?2, origin = 'user', status = 'confirmed' WHERE id = ?1",
            params![id, kind],
        );
        match n {
            Ok(0) => Err(Error::NotFound),
            Ok(_) => Ok(()),
            // The same pair may already carry this kind.
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
                Err(Error::Invalid("these articles already have a link of that type".into()))
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Records a relevance classification. A user decision on the same
    /// article and session is never overwritten by AI output.
    pub fn record_relevance(
        &self,
        article: ArticleId,
        session: Option<i64>,
        class: &str,
        reason: Option<&str>,
        origin: Origin,
        suggestion: Option<i64>,
    ) -> Result<()> {
        if !RELEVANCE_CLASSES.contains(&class) {
            return Err(Error::Invalid(format!("unknown relevance class {class}")));
        }
        let status = if origin == Origin::User { Status::Confirmed } else { Status::Suggested };
        self.conn().execute(
            "INSERT INTO article_relevance(article_id, session_id, class, reason, origin, status, suggestion_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(article_id, session_id) DO UPDATE SET
               class = CASE WHEN article_relevance.origin = 'user' AND excluded.origin = 'ai' THEN article_relevance.class ELSE excluded.class END,
               reason = CASE WHEN article_relevance.origin = 'user' AND excluded.origin = 'ai' THEN article_relevance.reason ELSE excluded.reason END,
               status = CASE WHEN article_relevance.origin = 'user' AND excluded.origin = 'ai' THEN article_relevance.status ELSE excluded.status END,
               origin = CASE WHEN article_relevance.origin = 'user' THEN 'user' ELSE excluded.origin END",
            params![article, session, class, reason, origin, status, suggestion, now()],
        )?;
        Ok(())
    }

    pub fn session_relevance(&self, session: i64, only_pending: bool, limit: u32) -> Result<Vec<RelevanceRow>> {
        let sql = format!(
            "SELECT r.id, a.id, a.title, r.class, r.reason, r.status
             FROM article_relevance r JOIN articles a ON a.id = r.article_id
             WHERE r.session_id = ?1 AND r.status {}
             ORDER BY CASE r.class WHEN 'relevant' THEN 0 WHEN 'possibly_relevant' THEN 1 ELSE 2 END,
                      a.effective_at DESC LIMIT ?2",
            status_filter(only_pending)
        );
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![session, limit], |r| {
            Ok(RelevanceRow {
                id: r.get(0)?,
                article_id: r.get(1)?,
                article_title: r.get(2)?,
                class: r.get(3)?,
                reason: r.get(4)?,
                status: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn decide_relevance(&self, id: i64, status: Status) -> Result<()> {
        let n = self.conn().execute("UPDATE article_relevance SET status = ?2 WHERE id = ?1", params![id, status])?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    /// Applies one decision to every pending item of a kind in a session.
    pub fn bulk_decide_session(&self, session: i64, kind: ReviewKind, status: Status) -> Result<usize> {
        let sql = match kind {
            ReviewKind::Entities => {
                "UPDATE article_entities SET status = ?2
                 WHERE status = 'suggested' AND suggestion_id IN (SELECT id FROM ai_suggestions WHERE session_id = ?1)"
            }
            ReviewKind::Relationships => {
                "UPDATE article_relationships SET status = ?2
                 WHERE status = 'suggested' AND suggestion_id IN (SELECT id FROM ai_suggestions WHERE session_id = ?1)"
            }
            ReviewKind::Relevance => {
                "UPDATE article_relevance SET status = ?2 WHERE status = 'suggested' AND session_id = ?1"
            }
        };
        Ok(self.conn().execute(sql, params![session, status])?)
    }

    /// The stored research summary of a session, if any (latest wins).
    pub fn session_summary(&self, session: i64) -> Result<Option<serde_json::Value>> {
        let json: Option<String> = self
            .conn()
            .query_row(
                "SELECT payload_json FROM ai_suggestions WHERE session_id = ?1 AND kind = 'summary' ORDER BY id DESC LIMIT 1",
                [session],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json.map(|j| serde_json::from_str(&j)).transpose()?)
    }

    /// Titles for article ids (missing ids are skipped).
    pub fn article_titles(&self, ids: &[ArticleId]) -> Result<Vec<(ArticleId, String)>> {
        let mut stmt = self.conn().prepare_cached("SELECT title FROM articles WHERE id = ?1")?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(t) = stmt.query_row([id], |r| r.get::<_, String>(0)).optional()? {
                out.push((*id, t));
            }
        }
        Ok(out)
    }
}

/// A stored location with its provenance, for the map and places list.
#[derive(Debug, Clone, PartialEq)]
pub struct LocationRow {
    pub id: i64,
    pub article_id: ArticleId,
    pub article_title: String,
    pub place_name: String,
    pub country: Option<String>,
    /// `(latitude, longitude)` when known.
    pub coordinates: Option<(f64, f64)>,
    pub role: crate::LocationRole,
    pub evidence: Option<String>,
    pub coordinate_source: Option<String>,
    pub ambiguous: bool,
    pub origin: Origin,
    pub status: Status,
}

impl Store {
    /// Locations, optionally limited to one session's articles. Rejected
    /// locations are left out.
    pub fn locations(&self, session: Option<i64>, limit: u32) -> Result<Vec<LocationRow>> {
        let mut stmt = self.conn().prepare(
            "SELECT l.id, a.id, a.title, l.place_name, l.country, l.latitude, l.longitude, l.role, l.evidence,
                    l.coordinate_source, l.ambiguous, l.origin, l.status
             FROM article_locations l JOIN articles a ON a.id = l.article_id
             WHERE l.status <> 'rejected'
               AND (?1 IS NULL OR l.article_id IN (SELECT article_id FROM analysis_session_articles WHERE session_id = ?1))
             ORDER BY l.latitude IS NULL, l.place_name COLLATE NOCASE, a.effective_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session, limit], |r| {
            let (lat, lon): (Option<f64>, Option<f64>) = (r.get(5)?, r.get(6)?);
            Ok(LocationRow {
                id: r.get(0)?,
                article_id: r.get(1)?,
                article_title: r.get(2)?,
                place_name: r.get(3)?,
                country: r.get(4)?,
                coordinates: lat.zip(lon),
                role: r.get(7)?,
                evidence: r.get(8)?,
                coordinate_source: r.get(9)?,
                ambiguous: r.get(10)?,
                origin: r.get(11)?,
                status: r.get(12)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Sets (or clears) coordinates placed by the user. Placing a location
    /// is a user verification, so it also confirms it and clears the
    /// ambiguity flag; clearing coordinates leaves the status unchanged.
    pub fn set_location_coordinates(&self, id: i64, coordinates: Option<(f64, f64)>) -> Result<()> {
        let n = match coordinates {
            Some((lat, lon)) => {
                if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) || !lat.is_finite() || !lon.is_finite() {
                    return Err(Error::Invalid("coordinates out of range".into()));
                }
                self.conn().execute(
                    "UPDATE article_locations SET latitude = ?2, longitude = ?3, coordinate_source = 'user',
                            status = 'confirmed', ambiguous = 0 WHERE id = ?1",
                    params![id, lat, lon],
                )?
            }
            None => self.conn().execute(
                "UPDATE article_locations SET latitude = NULL, longitude = NULL, coordinate_source = NULL WHERE id = ?1",
                [id],
            )?,
        };
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    pub fn decide_location(&self, id: i64, status: Status) -> Result<()> {
        let n = self.conn().execute("UPDATE article_locations SET status = ?2 WHERE id = ?1", params![id, status])?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }
}

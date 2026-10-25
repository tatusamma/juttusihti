//! Research data model: labels, entities, events, typed relationships,
//! annotations, AI suggestions and analysis sessions.
//!
//! Invariant: AI-originated records are written with `Status::Suggested`
//! and only an explicit user decision moves them to confirmed or rejected.
//! Nothing here modifies the source article columns.

use rusqlite::{OptionalExtension, params};

use crate::normalize::normalize_text;
use crate::store::Store;
use crate::{
    ArticleId, ArticleLabel, Entity, EntityId, EntityKind, Error, Event, EventId, Label, LabelId, LocationRole, Origin,
    Relationship, RelationshipKind, Result, SessionStatus, Status, now,
};

fn initial_status(origin: Origin) -> Status {
    match origin {
        Origin::User => Status::Confirmed,
        Origin::Ai => Status::Suggested,
    }
}

/// A new location record. Coordinates are optional: an AI-produced place
/// name is not a verified coordinate.
#[derive(Debug, Clone, PartialEq)]
pub struct NewLocation {
    pub place_name: String,
    pub country: Option<String>,
    pub coordinates: Option<(f64, f64)>,
    pub role: LocationRole,
    pub evidence: Option<String>,
    pub confidence: Option<f64>,
    pub coordinate_source: Option<String>,
    pub ambiguous: bool,
}

/// An AI suggestion with provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSuggestion {
    pub session_id: Option<i64>,
    pub kind: String,
    pub payload: serde_json::Value,
    pub provider: String,
    pub model: String,
    pub prompt_version: String,
    pub confidence: Option<f64>,
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub id: i64,
    pub status: SessionStatus,
    pub config: serde_json::Value,
    pub estimate: Option<serde_json::Value>,
    pub error: Option<String>,
}

impl SessionStatus {
    /// Allowed lifecycle transitions.
    pub fn can_become(self, next: SessionStatus) -> bool {
        use SessionStatus::*;
        matches!(
            (self, next),
            (Draft, Estimated)
                | (Estimated, Estimated | AwaitingApproval | Draft | Cancelled)
                | (AwaitingApproval, Running | Draft | Cancelled)
                | (Running, Paused | Cancelled | Failed | Completed)
                | (Paused, Running | Cancelled)
                | (Failed, Running | Cancelled)
        )
    }
}

/// Levenshtein distance over chars, for label similarity checks.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != *cb)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Whether two normalized labels are likely the same concept: equal, a
/// small edit apart, or one an inflected/compound form of the other
/// (common in Finnish: "vaali" / "vaalit" / "eduskuntavaalit").
fn similar_labels(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let (short, long) = if a.chars().count() <= b.chars().count() { (a, b) } else { (b, a) };
    let n = short.chars().count();
    (n >= 4 && long.starts_with(short) && long.chars().count() - n <= 3) || (n >= 6 && edit_distance(a, b) <= 2)
}

impl Store {
    // ---- labels --------------------------------------------------------

    /// Creates a label or returns the existing one with the same normalized name.
    pub fn create_label(&self, name: &str) -> Result<LabelId> {
        let name = name.trim();
        let normalized = normalize_text(name);
        if normalized.is_empty() {
            return Err(Error::Invalid("label name must contain letters or digits".into()));
        }
        self.conn().execute(
            "INSERT INTO labels(name, normalized, created_at) VALUES (?1, ?2, ?3) ON CONFLICT(normalized) DO NOTHING",
            params![name, normalized, now()],
        )?;
        Ok(self.conn().query_row("SELECT id FROM labels WHERE normalized = ?1", [normalized], |r| r.get(0))?)
    }

    pub fn labels(&self) -> Result<Vec<Label>> {
        let mut stmt = self.conn().prepare("SELECT id, name, color FROM labels ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| Ok(Label { id: r.get(0)?, name: r.get(1)?, color: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Labels with (confirmed, pending suggestion) article counts.
    pub fn label_counts(&self) -> Result<Vec<(Label, u64, u64)>> {
        let mut stmt = self.conn().prepare(
            "SELECT l.id, l.name, l.color,
                    count(al.article_id) FILTER (WHERE al.status = 'confirmed'),
                    count(al.article_id) FILTER (WHERE al.status = 'suggested')
             FROM labels l LEFT JOIN article_labels al ON al.label_id = l.id
             GROUP BY l.id ORDER BY l.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                Label { id: r.get(0)?, name: r.get(1)?, color: r.get(2)? },
                r.get::<_, i64>(3)? as u64,
                r.get::<_, i64>(4)? as u64,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Existing labels that a proposed name probably duplicates. Used to
    /// steer AI suggestions toward the existing vocabulary.
    pub fn similar_labels(&self, name: &str) -> Result<Vec<Label>> {
        let wanted = normalize_text(name);
        let mut stmt = self.conn().prepare("SELECT id, name, color, normalized FROM labels")?;
        let rows = stmt.query_map([], |r| {
            Ok((Label { id: r.get(0)?, name: r.get(1)?, color: r.get(2)? }, r.get::<_, String>(3)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (label, normalized) = row?;
            if similar_labels(&wanted, &normalized) {
                out.push(label);
            }
        }
        Ok(out)
    }

    pub fn rename_label(&self, id: LabelId, name: &str) -> Result<()> {
        let normalized = normalize_text(name);
        if normalized.is_empty() {
            return Err(Error::Invalid("label name must contain letters or digits".into()));
        }
        let changed = self.conn().execute(
            "UPDATE labels SET name = ?1, normalized = ?2 WHERE id = ?3",
            params![name.trim(), normalized, id],
        )?;
        if changed == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    /// Merges `from` into `into`: associations move over (keeping the
    /// stronger status when both exist) and `from` is deleted.
    pub fn merge_labels(&mut self, from: LabelId, into: LabelId) -> Result<()> {
        if from == into {
            return Ok(());
        }
        let tx = self.conn_mut().transaction()?;
        tx.execute(
            "INSERT INTO article_labels(article_id, label_id, origin, status, suggestion_id, created_at)
             SELECT article_id, ?2, origin, status, suggestion_id, created_at FROM article_labels WHERE label_id = ?1
             ON CONFLICT(article_id, label_id) DO UPDATE SET
               status = CASE WHEN excluded.status = 'confirmed' OR article_labels.status = 'confirmed' THEN 'confirmed'
                             WHEN excluded.status = 'suggested' OR article_labels.status = 'suggested' THEN 'suggested'
                             ELSE 'rejected' END",
            params![from, into],
        )?;
        if tx.execute("DELETE FROM labels WHERE id = ?1", [from])? == 0 {
            return Err(Error::NotFound);
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_label(&self, id: LabelId) -> Result<()> {
        let n = self.conn().execute("DELETE FROM labels WHERE id = ?1", [id])?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    /// Attaches a label. A user action always wins over an existing AI
    /// suggestion; an AI suggestion never overrides a user decision.
    pub fn tag_article(
        &self,
        article: ArticleId,
        label: LabelId,
        origin: Origin,
        suggestion: Option<i64>,
    ) -> Result<()> {
        let status = initial_status(origin);
        self.conn().execute(
            "INSERT INTO article_labels(article_id, label_id, origin, status, suggestion_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(article_id, label_id) DO UPDATE SET
               origin = CASE WHEN excluded.origin = 'user' THEN 'user' ELSE article_labels.origin END,
               status = CASE WHEN excluded.origin = 'user' THEN excluded.status ELSE article_labels.status END",
            params![article, label, origin, status, suggestion, now()],
        )?;
        Ok(())
    }

    /// Confirms or rejects a label on an article (user decision).
    pub fn decide_label(&self, article: ArticleId, label: LabelId, status: Status) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE article_labels SET status = ?3 WHERE article_id = ?1 AND label_id = ?2",
            params![article, label, status],
        )?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    /// Applies one decision to every pending AI suggestion of a label.
    pub fn bulk_decide_label(&self, label: LabelId, status: Status) -> Result<usize> {
        Ok(self.conn().execute(
            "UPDATE article_labels SET status = ?2 WHERE label_id = ?1 AND status = 'suggested'",
            params![label, status],
        )?)
    }

    pub fn untag_article(&self, article: ArticleId, label: LabelId) -> Result<()> {
        self.conn()
            .execute("DELETE FROM article_labels WHERE article_id = ?1 AND label_id = ?2", params![article, label])?;
        Ok(())
    }

    pub fn article_labels(&self, article: ArticleId) -> Result<Vec<ArticleLabel>> {
        let mut stmt = self.conn().prepare(
            "SELECT l.id, l.name, l.color, al.origin, al.status FROM article_labels al
             JOIN labels l ON l.id = al.label_id WHERE al.article_id = ?1 ORDER BY l.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([article], |r| {
            Ok(ArticleLabel {
                label: Label { id: r.get(0)?, name: r.get(1)?, color: r.get(2)? },
                origin: r.get(3)?,
                status: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- entities ------------------------------------------------------

    /// Finds an entity by name or alias, or creates it. Resolution is
    /// conservative: only an exact normalized match of the same kind counts.
    pub fn resolve_entity(&self, kind: EntityKind, name: &str) -> Result<EntityId> {
        let normalized = normalize_text(name);
        if normalized.is_empty() {
            return Err(Error::Invalid("entity name must contain letters or digits".into()));
        }
        let existing: Option<EntityId> = self
            .conn()
            .query_row(
                "SELECT id FROM entities WHERE kind = ?1 AND normalized = ?2
                 UNION SELECT e.id FROM entity_aliases a JOIN entities e ON e.id = a.entity_id
                       WHERE e.kind = ?1 AND a.normalized = ?2
                 LIMIT 1",
                params![kind, normalized],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok(id);
        }
        self.conn().execute(
            "INSERT INTO entities(kind, name, normalized, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![kind, name.trim(), normalized, now()],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn add_entity_alias(&self, entity: EntityId, alias: &str) -> Result<()> {
        let normalized = normalize_text(alias);
        if normalized.is_empty() {
            return Err(Error::Invalid("alias must contain letters or digits".into()));
        }
        self.conn().execute(
            "INSERT INTO entity_aliases(entity_id, alias, normalized) VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING",
            params![entity, alias.trim(), normalized],
        )?;
        Ok(())
    }

    pub fn entities(&self) -> Result<Vec<Entity>> {
        let mut stmt = self.conn().prepare("SELECT id, kind, name FROM entities ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| Ok(Entity { id: r.get(0)?, kind: r.get(1)?, name: r.get(2)? }))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Records that an article mentions an entity.
    pub fn link_entity(
        &self,
        article: ArticleId,
        entity: EntityId,
        role: &str,
        evidence: Option<&str>,
        origin: Origin,
        suggestion: Option<i64>,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO article_entities(article_id, entity_id, role, evidence, origin, status, suggestion_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(article_id, entity_id, role) DO UPDATE SET
               origin = CASE WHEN excluded.origin = 'user' THEN 'user' ELSE article_entities.origin END,
               status = CASE WHEN excluded.origin = 'user' THEN excluded.status ELSE article_entities.status END",
            params![article, entity, role, evidence, origin, initial_status(origin), suggestion, now()],
        )?;
        Ok(())
    }

    /// Merges `from` into `into`: mentions and aliases move; `from`'s name
    /// becomes an alias of `into`.
    pub fn merge_entities(&mut self, from: EntityId, into: EntityId) -> Result<()> {
        if from == into {
            return Ok(());
        }
        let tx = self.conn_mut().transaction()?;
        let (name, kind_from): (String, String) = tx
            .query_row("SELECT name, kind FROM entities WHERE id = ?1", [from], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
            .ok_or(Error::NotFound)?;
        let kind_into: String = tx
            .query_row("SELECT kind FROM entities WHERE id = ?1", [into], |r| r.get(0))
            .optional()?
            .ok_or(Error::NotFound)?;
        if kind_from != kind_into {
            return Err(Error::Invalid("cannot merge entities of different kinds".into()));
        }
        tx.execute(
            "INSERT OR IGNORE INTO article_entities(article_id, entity_id, role, evidence, origin, status, suggestion_id, created_at)
             SELECT article_id, ?2, role, evidence, origin, status, suggestion_id, created_at FROM article_entities WHERE entity_id = ?1",
            params![from, into],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO entity_aliases(entity_id, alias, normalized)
             SELECT ?2, alias, normalized FROM entity_aliases WHERE entity_id = ?1",
            params![from, into],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO entity_aliases(entity_id, alias, normalized) VALUES (?1, ?2, ?3)",
            params![into, name, normalize_text(&name)],
        )?;
        tx.execute("DELETE FROM entities WHERE id = ?1", [from])?;
        tx.commit()?;
        Ok(())
    }

    /// Moves one article mention to a different entity, to correct a wrong merge.
    pub fn reassign_mention(&self, mention_id: i64, entity: EntityId) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE article_entities SET entity_id = ?2, origin = 'user', status = 'confirmed' WHERE id = ?1",
            params![mention_id, entity],
        )?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    /// (mention id, entity, role, status) for an article.
    pub fn article_entities(&self, article: ArticleId) -> Result<Vec<(i64, Entity, String, Status)>> {
        let mut stmt = self.conn().prepare(
            "SELECT ae.id, e.id, e.kind, e.name, ae.role, ae.status FROM article_entities ae
             JOIN entities e ON e.id = ae.entity_id WHERE ae.article_id = ?1 ORDER BY e.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([article], |r| {
            Ok((r.get(0)?, Entity { id: r.get(1)?, kind: r.get(2)?, name: r.get(3)? }, r.get(4)?, r.get(5)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- events --------------------------------------------------------

    pub fn create_event(
        &self,
        title: &str,
        description: Option<&str>,
        starts_at: Option<i64>,
        ends_at: Option<i64>,
        origin: Origin,
    ) -> Result<EventId> {
        if title.trim().is_empty() {
            return Err(Error::Invalid("event title must not be empty".into()));
        }
        if let (Some(s), Some(e)) = (starts_at, ends_at)
            && e < s
        {
            return Err(Error::Invalid("event ends before it starts".into()));
        }
        self.conn().execute(
            "INSERT INTO events(title, description, starts_at, ends_at, origin, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![title.trim(), description, starts_at, ends_at, origin, initial_status(origin), now()],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn events(&self) -> Result<Vec<Event>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, title, description, starts_at, ends_at, origin, status FROM events
             ORDER BY starts_at IS NULL, starts_at, id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Event {
                id: r.get(0)?,
                title: r.get(1)?,
                description: r.get(2)?,
                starts_at: r.get(3)?,
                ends_at: r.get(4)?,
                origin: r.get(5)?,
                status: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn link_event(
        &self,
        article: ArticleId,
        event: EventId,
        origin: Origin,
        suggestion: Option<i64>,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO article_events(article_id, event_id, origin, status, suggestion_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(article_id, event_id) DO UPDATE SET
               origin = CASE WHEN excluded.origin = 'user' THEN 'user' ELSE article_events.origin END,
               status = CASE WHEN excluded.origin = 'user' THEN excluded.status ELSE article_events.status END",
            params![article, event, origin, initial_status(origin), suggestion, now()],
        )?;
        Ok(())
    }

    // ---- relationships -------------------------------------------------

    /// Records a typed relationship. Symmetric kinds are stored with the
    /// smaller id first so A–B and B–A are the same row.
    #[allow(clippy::too_many_arguments)]
    pub fn relate(
        &self,
        source: ArticleId,
        target: ArticleId,
        kind: RelationshipKind,
        origin: Origin,
        evidence: Option<&str>,
        confidence: Option<f64>,
        suggestion: Option<i64>,
    ) -> Result<i64> {
        if source == target {
            return Err(Error::Invalid("an article cannot relate to itself".into()));
        }
        if confidence.is_some_and(|c| !(0.0..=1.0).contains(&c)) {
            return Err(Error::Invalid("confidence must be between 0 and 1".into()));
        }
        let symmetric = matches!(
            kind,
            RelationshipKind::Duplicate
                | RelationshipKind::SameEvent
                | RelationshipKind::Related
                | RelationshipKind::ConflictingClaim
        );
        let (s, t) = if symmetric && source > target { (target, source) } else { (source, target) };
        self.conn().execute(
            "INSERT INTO article_relationships(source_id, target_id, kind, origin, status, evidence, confidence, suggestion_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(source_id, target_id, kind) DO UPDATE SET
               origin = CASE WHEN excluded.origin = 'user' THEN 'user' ELSE article_relationships.origin END,
               status = CASE WHEN excluded.origin = 'user' THEN excluded.status ELSE article_relationships.status END,
               evidence = COALESCE(article_relationships.evidence, excluded.evidence)",
            params![s, t, kind, origin, initial_status(origin), evidence, confidence, suggestion, now()],
        )?;
        Ok(self.conn().query_row(
            "SELECT id FROM article_relationships WHERE source_id = ?1 AND target_id = ?2 AND kind = ?3",
            params![s, t, kind],
            |r| r.get(0),
        )?)
    }

    pub fn decide_relationship(&self, id: i64, status: Status) -> Result<()> {
        let n =
            self.conn().execute("UPDATE article_relationships SET status = ?2 WHERE id = ?1", params![id, status])?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    pub fn remove_relationship(&self, id: i64) -> Result<()> {
        let n = self.conn().execute("DELETE FROM article_relationships WHERE id = ?1", [id])?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    /// Relationships touching an article, excluding rejected ones.
    pub fn relationships(&self, article: ArticleId) -> Result<Vec<Relationship>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, source_id, target_id, kind, origin, status, evidence, confidence FROM article_relationships
             WHERE (source_id = ?1 OR target_id = ?1) AND status <> 'rejected' ORDER BY id",
        )?;
        let rows = stmt.query_map([article], |r| {
            Ok(Relationship {
                id: r.get(0)?,
                source_id: r.get(1)?,
                target_id: r.get(2)?,
                kind: r.get(3)?,
                origin: r.get(4)?,
                status: r.get(5)?,
                evidence: r.get(6)?,
                confidence: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- locations -----------------------------------------------------

    pub fn add_location(&self, article: ArticleId, loc: &NewLocation, origin: Origin) -> Result<i64> {
        if loc.place_name.trim().is_empty() {
            return Err(Error::Invalid("place name must not be empty".into()));
        }
        if let Some((lat, lon)) = loc.coordinates {
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                return Err(Error::Invalid("coordinates out of range".into()));
            }
            if loc.coordinate_source.is_none() {
                return Err(Error::Invalid("coordinates need a recorded source".into()));
            }
        }
        self.conn().execute(
            "INSERT INTO article_locations(article_id, place_name, country, latitude, longitude, role, evidence,
                                           confidence, coordinate_source, ambiguous, origin, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                article,
                loc.place_name.trim(),
                loc.country,
                loc.coordinates.map(|c| c.0),
                loc.coordinates.map(|c| c.1),
                loc.role,
                loc.evidence,
                loc.confidence,
                loc.coordinate_source,
                loc.ambiguous,
                origin,
                initial_status(origin),
                now()
            ],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    // ---- annotations ---------------------------------------------------

    pub fn annotate(&self, article: ArticleId, session: Option<i64>, body: &str) -> Result<i64> {
        if body.trim().is_empty() {
            return Err(Error::Invalid("annotation must not be empty".into()));
        }
        let t = now();
        self.conn().execute(
            "INSERT INTO annotations(article_id, session_id, body, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![article, session, body, t],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn annotations(&self, article: ArticleId) -> Result<Vec<(i64, String)>> {
        let mut stmt =
            self.conn().prepare("SELECT id, body FROM annotations WHERE article_id = ?1 ORDER BY created_at, id")?;
        let rows = stmt.query_map([article], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- AI suggestions ------------------------------------------------

    pub fn record_suggestion(&self, s: &NewSuggestion) -> Result<i64> {
        self.conn().execute(
            "INSERT INTO ai_suggestions(session_id, kind, payload_json, provider, model, prompt_version, confidence,
                                        evidence, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'suggested', ?9)",
            params![
                s.session_id,
                s.kind,
                serde_json::to_string(&s.payload)?,
                s.provider,
                s.model,
                s.prompt_version,
                s.confidence,
                s.evidence,
                now()
            ],
        )?;
        Ok(self.conn().last_insert_rowid())
    }

    pub fn decide_suggestion(&self, id: i64, status: Status) -> Result<()> {
        if status == Status::Suggested {
            return Err(Error::Invalid("a decision must confirm or reject".into()));
        }
        let n = self.conn().execute(
            "UPDATE ai_suggestions SET status = ?2, decided_at = ?3 WHERE id = ?1",
            params![id, status, now()],
        )?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    // ---- analysis sessions ---------------------------------------------

    /// Creates a draft session with a stable snapshot of its article ids.
    pub fn create_session(&mut self, config: &serde_json::Value, articles: &[ArticleId]) -> Result<i64> {
        let tx = self.conn_mut().transaction()?;
        tx.execute(
            "INSERT INTO analysis_sessions(created_at, status, config_json) VALUES (?1, 'draft', ?2)",
            params![now(), serde_json::to_string(config)?],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut ins =
                tx.prepare("INSERT OR IGNORE INTO analysis_session_articles(session_id, article_id) VALUES (?1, ?2)")?;
            for a in articles {
                ins.execute(params![id, a])?;
            }
        }
        tx.commit()?;
        Ok(id)
    }

    pub fn session(&self, id: i64) -> Result<SessionRecord> {
        let row: Option<(SessionStatus, String, Option<String>, Option<String>)> = self
            .conn()
            .query_row(
                "SELECT status, config_json, estimate_json, error FROM analysis_sessions WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let (status, config, estimate, error) = row.ok_or(Error::NotFound)?;
        Ok(SessionRecord {
            id,
            status,
            config: serde_json::from_str(&config)?,
            estimate: estimate.map(|e| serde_json::from_str(&e)).transpose()?,
            error,
        })
    }

    pub fn session_articles(&self, id: i64) -> Result<Vec<ArticleId>> {
        let mut stmt = self
            .conn()
            .prepare("SELECT article_id FROM analysis_session_articles WHERE session_id = ?1 ORDER BY article_id")?;
        let rows = stmt.query_map([id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Moves a session to a new status, enforcing the lifecycle.
    pub fn set_session_status(&self, id: i64, next: SessionStatus, error: Option<&str>) -> Result<()> {
        let current = self.session(id)?.status;
        if !current.can_become(next) {
            return Err(Error::Invalid(format!("session cannot go from {} to {}", current.as_str(), next.as_str())));
        }
        let finished = matches!(next, SessionStatus::Completed | SessionStatus::Cancelled | SessionStatus::Failed);
        self.conn().execute(
            "UPDATE analysis_sessions SET status = ?2, error = ?3,
               completed_at = CASE WHEN ?4 THEN ?5 ELSE completed_at END WHERE id = ?1",
            params![id, next, error, finished, now()],
        )?;
        Ok(())
    }

    pub fn set_session_estimate(&self, id: i64, estimate: &serde_json::Value) -> Result<()> {
        self.set_session_status(id, SessionStatus::Estimated, None)?;
        self.conn().execute(
            "UPDATE analysis_sessions SET estimate_json = ?2 WHERE id = ?1",
            params![id, serde_json::to_string(estimate)?],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_similarity() {
        assert!(similar_labels("vaali", "vaalit"));
        assert!(similar_labels("climate change", "climate changes"));
        assert!(!similar_labels("sää", "sääli"), "short words need exact match");
        assert!(!similar_labels("talous", "terveys"));
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn session_lifecycle() {
        use SessionStatus::*;
        assert!(Draft.can_become(Estimated));
        assert!(!Draft.can_become(Running), "cannot run without estimate and approval");
        assert!(!Estimated.can_become(Running));
        assert!(AwaitingApproval.can_become(Running));
        assert!(Paused.can_become(Running));
        assert!(!Completed.can_become(Running));
        assert!(!Cancelled.can_become(Running));
    }
}

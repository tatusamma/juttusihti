//! Organising API: category groups, ordering, publisher term mapping,
//! favourites, the category tree and per-category counts.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};

use crate::normalize::normalize_text;
use crate::search::{SearchQuery, build_where};
use crate::store::{ListFilter, Store};
use crate::{ArticleId, Error, Label, LabelId, Result, View, now};

pub type GroupId = i64;
pub type TermId = i64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub id: GroupId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryNode {
    pub label: Label,
    pub group_id: Option<GroupId>,
    pub term_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupNode {
    pub group: Group,
    pub categories: Vec<CategoryNode>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Favourite {
    Group(GroupId),
    Category(LabelId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryTree {
    pub groups: Vec<GroupNode>,
    pub ungrouped: Vec<CategoryNode>,
    pub favourites: Vec<Favourite>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermRow {
    pub id: TermId,
    pub name: String,
    pub feed_title: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CategoryCounts {
    pub groups: HashMap<GroupId, u64>,
    pub categories: HashMap<LabelId, u64>,
    /// Articles in no category.
    pub uncategorized: u64,
}

/// An article offered to AI for categories: its headline and a short text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticleForCategorizing {
    pub id: ArticleId,
    pub title: String,
    /// The description, or the start of the content when there is none.
    pub text: String,
}

/// What [`Store::apply_article_categories`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyStats {
    /// Articles now confirmed in at least one of the given categories.
    pub categorized: usize,
    /// Categories that did not exist before.
    pub new_categories: usize,
}

/// Characters of content sent when an article has no description.
const CONTENT_START: usize = 300;

fn clean_group_name(name: &str) -> Result<(String, String)> {
    let name = name.trim().to_string();
    let normalized = normalize_text(&name);
    if normalized.is_empty() {
        return Err(Error::Invalid("group name must contain letters or digits".into()));
    }
    Ok((name, normalized))
}

fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation)
}

fn favourite_column(f: Favourite) -> (&'static str, i64) {
    match f {
        Favourite::Group(id) => ("group_id", id),
        Favourite::Category(id) => ("label_id", id),
    }
}

fn node_from_row(r: &rusqlite::Row) -> rusqlite::Result<CategoryNode> {
    Ok(CategoryNode {
        label: Label { id: r.get(0)?, name: r.get(1)?, color: r.get(2)? },
        group_id: r.get(3)?,
        term_count: r.get::<_, i64>(4)? as u64,
    })
}

/// Orders terms by name, then feed.
const BY_NAME: &str = "t.name COLLATE NOCASE, f.title COLLATE NOCASE, t.id";

/// Orders terms by how many articles carry them, most first, then by name.
pub(crate) const BY_ARTICLES: &str = "(SELECT COUNT(*) FROM article_terms x WHERE x.term_id = t.id) DESC,
     t.name COLLATE NOCASE, f.title COLLATE NOCASE, t.id";

fn terms(conn: &Connection, cond: &str, order: &str, label: Option<LabelId>) -> Result<Vec<TermRow>> {
    let sql = format!(
        "SELECT t.id, t.name, f.title FROM feed_terms t JOIN feeds f ON f.id = t.feed_id
         WHERE {cond} ORDER BY {order}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let map = |r: &rusqlite::Row| Ok(TermRow { id: r.get(0)?, name: r.get(1)?, feed_title: r.get(2)? });
    let rows = match label {
        Some(l) => stmt.query_map([l], map)?.collect::<rusqlite::Result<_>>()?,
        None => stmt.query_map([], map)?.collect::<rusqlite::Result<_>>()?,
    };
    Ok(rows)
}

fn column(conn: &Connection, sql: &str, p: impl rusqlite::Params) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(sql)?;
    Ok(stmt.query_map(p, |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
}

impl Store {
    pub fn category_tree(&self) -> Result<CategoryTree> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT l.id, l.name, l.color, l.group_id,
                    (SELECT COUNT(*) FROM feed_terms t WHERE t.label_id = l.id)
             FROM labels l ORDER BY l.position, l.name COLLATE NOCASE, l.id",
        )?;
        let nodes: Vec<CategoryNode> = stmt.query_map([], node_from_row)?.collect::<rusqlite::Result<_>>()?;
        let mut stmt =
            conn.prepare("SELECT id, name FROM category_groups ORDER BY position, name COLLATE NOCASE, id")?;
        let mut groups: Vec<GroupNode> = stmt
            .query_map([], |r| Ok(GroupNode { group: Group { id: r.get(0)?, name: r.get(1)? }, categories: vec![] }))?
            .collect::<rusqlite::Result<_>>()?;
        let mut ungrouped = Vec::new();
        for n in nodes {
            match n.group_id.and_then(|g| groups.iter_mut().find(|x| x.group.id == g)) {
                Some(g) => g.categories.push(n),
                None => ungrouped.push(n),
            }
        }
        let mut stmt = conn.prepare("SELECT group_id, label_id FROM favourites ORDER BY position, id")?;
        let favourites = stmt
            .query_map([], |r| {
                Ok(match r.get::<_, Option<GroupId>>(0)? {
                    Some(g) => Favourite::Group(g),
                    None => Favourite::Category(r.get(1)?),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(CategoryTree { groups, ungrouped, favourites })
    }

    pub fn create_group(&self, name: &str) -> Result<GroupId> {
        let (name, normalized) = clean_group_name(name)?;
        match self.conn().execute(
            "INSERT INTO category_groups(name, normalized, position, created_at)
             VALUES (?1, ?2, (SELECT COALESCE(MAX(position) + 1, 0) FROM category_groups), ?3)",
            params![name, normalized, now()],
        ) {
            Ok(_) => Ok(self.conn().last_insert_rowid()),
            Err(e) if is_unique_violation(&e) => Err(Error::Invalid("a group with that name already exists".into())),
            Err(e) => Err(e.into()),
        }
    }

    pub fn rename_group(&self, id: GroupId, name: &str) -> Result<()> {
        let (name, normalized) = clean_group_name(name)?;
        match self.conn().execute(
            "UPDATE category_groups SET name = ?1, normalized = ?2 WHERE id = ?3",
            params![name, normalized, id],
        ) {
            Ok(0) => Err(Error::NotFound),
            Ok(_) => Ok(()),
            Err(e) if is_unique_violation(&e) => Err(Error::Invalid("a group with that name already exists".into())),
            Err(e) => Err(e.into()),
        }
    }

    /// Deletes a group; its categories go after the Ungrouped ones, in their order.
    pub fn delete_group(&mut self, id: GroupId) -> Result<()> {
        let tx = self.conn_mut().transaction()?;
        let next: i64 =
            tx.query_row("SELECT COALESCE(MAX(position) + 1, 0) FROM labels WHERE group_id IS NULL", [], |r| r.get(0))?;
        let freed =
            column(&tx, "SELECT id FROM labels WHERE group_id = ?1 ORDER BY position, name COLLATE NOCASE, id", [id])?;
        for (i, label) in freed.iter().enumerate() {
            tx.execute(
                "UPDATE labels SET group_id = NULL, position = ?1 WHERE id = ?2",
                params![next + i as i64, label],
            )?;
        }
        let n = tx.execute("DELETE FROM category_groups WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(Error::NotFound);
        }
        tx.commit()?;
        Ok(())
    }

    pub fn move_group(&mut self, id: GroupId, index: usize) -> Result<()> {
        let tx = self.conn_mut().transaction()?;
        let mut ids = column(&tx, "SELECT id FROM category_groups ORDER BY position, name COLLATE NOCASE, id", [])?;
        let from = ids.iter().position(|x| *x == id).ok_or(Error::NotFound)?;
        ids.remove(from);
        ids.insert(index.min(ids.len()), id);
        for (pos, gid) in ids.iter().enumerate() {
            tx.execute("UPDATE category_groups SET position = ?1 WHERE id = ?2", params![pos as i64, gid])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn move_category(&mut self, label: LabelId, group: Option<GroupId>, index: usize) -> Result<()> {
        let tx = self.conn_mut().transaction()?;
        let old: Option<GroupId> = tx
            .query_row("SELECT group_id FROM labels WHERE id = ?1", [label], |r| r.get(0))
            .optional()?
            .ok_or(Error::NotFound)?;
        if let Some(g) = group {
            let found: Option<i64> =
                tx.query_row("SELECT id FROM category_groups WHERE id = ?1", [g], |r| r.get(0)).optional()?;
            if found.is_none() {
                return Err(Error::NotFound);
            }
        }
        let siblings = |tx: &Connection, g: Option<GroupId>| {
            column(tx, "SELECT id FROM labels WHERE group_id IS ?1 ORDER BY position, name COLLATE NOCASE, id", [g])
        };
        let renumber = |tx: &Connection, ids: &[i64]| -> Result<()> {
            for (pos, id) in ids.iter().enumerate() {
                tx.execute("UPDATE labels SET position = ?1 WHERE id = ?2", params![pos as i64, id])?;
            }
            Ok(())
        };
        if old != group {
            let mut rest = siblings(&tx, old)?;
            rest.retain(|x| *x != label);
            renumber(&tx, &rest)?;
            tx.execute("UPDATE labels SET group_id = ?1 WHERE id = ?2", params![group, label])?;
        }
        let mut ids = siblings(&tx, group)?;
        ids.retain(|x| *x != label);
        ids.insert(index.min(ids.len()), label);
        renumber(&tx, &ids)?;
        tx.commit()?;
        Ok(())
    }

    pub fn category_terms(&self, label: LabelId) -> Result<Vec<TermRow>> {
        terms(self.conn(), "t.label_id = ?1", BY_NAME, Some(label))
    }

    /// The terms waiting to be sorted, the most frequent first.
    pub fn unsorted_terms(&self) -> Result<Vec<TermRow>> {
        terms(self.conn(), "t.label_id IS NULL AND t.ignored = 0", BY_ARTICLES, None)
    }

    pub fn ignored_terms(&self) -> Result<Vec<TermRow>> {
        terms(self.conn(), "t.ignored = 1", BY_NAME, None)
    }

    /// Maps a term to a category (or unmaps with `None`); clears `ignored`.
    pub fn map_term(&self, term: TermId, label: Option<LabelId>) -> Result<()> {
        let n = self
            .conn()
            .execute("UPDATE feed_terms SET label_id = ?1, ignored = 0 WHERE id = ?2", params![label, term])?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    /// Ignoring a term clears its mapping.
    pub fn set_term_ignored(&self, term: TermId, ignored: bool) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE feed_terms SET ignored = ?1, label_id = CASE WHEN ?1 = 1 THEN NULL ELSE label_id END WHERE id = ?2",
            params![ignored, term],
        )?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    pub fn add_favourite(&self, f: Favourite) -> Result<()> {
        let (col, id) = favourite_column(f);
        self.conn().execute(
            &format!(
                "INSERT OR IGNORE INTO favourites({col}, position)
                 VALUES (?1, (SELECT COALESCE(MAX(position) + 1, 0) FROM favourites))"
            ),
            [id],
        )?;
        Ok(())
    }

    pub fn remove_favourite(&self, f: Favourite) -> Result<()> {
        let (col, id) = favourite_column(f);
        self.conn().execute(&format!("DELETE FROM favourites WHERE {col} = ?1"), [id])?;
        Ok(())
    }

    pub fn move_favourite(&mut self, f: Favourite, index: usize) -> Result<()> {
        let (col, id) = favourite_column(f);
        let tx = self.conn_mut().transaction()?;
        let row: i64 = tx
            .query_row(&format!("SELECT id FROM favourites WHERE {col} = ?1"), [id], |r| r.get(0))
            .optional()?
            .ok_or(Error::NotFound)?;
        let mut ids = column(&tx, "SELECT id FROM favourites ORDER BY position, id", [])?;
        ids.retain(|x| *x != row);
        ids.insert(index.min(ids.len()), row);
        for (pos, fid) in ids.iter().enumerate() {
            tx.execute("UPDATE favourites SET position = ?1 WHERE id = ?2", params![pos as i64, fid])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Distinct article counts per category and per group (confirmed labels
    /// plus mapped publisher terms), restricted like a list. One statement:
    /// the memberships are worked out once and the counts read them.
    pub fn category_counts(&self, filter: ListFilter) -> Result<CategoryCounts> {
        let view = match filter.view {
            View::All => "",
            View::Unread => " AND a.is_read = 0",
            View::Saved => " AND a.is_saved = 1",
        };
        let sql = format!(
            "WITH m(article_id, label_id) AS (
               SELECT article_id, label_id FROM article_labels WHERE status = 'confirmed'
               UNION
               SELECT t.article_id, ft.label_id FROM article_terms t JOIN feed_terms ft ON ft.id = t.term_id
               WHERE ft.label_id IS NOT NULL AND NOT EXISTS (
                 SELECT 1 FROM article_labels r
                 WHERE r.article_id = t.article_id AND r.label_id = ft.label_id AND r.status = 'rejected')
             ),
             scoped(article_id, label_id) AS MATERIALIZED (
               SELECT m.article_id, m.label_id FROM m
               JOIN articles a ON a.id = m.article_id JOIN feeds f ON f.id = a.feed_id
               WHERE (?1 IS NULL OR a.feed_id = ?1) AND (?2 IS NULL OR f.folder_id = ?2){view}
             )
             SELECT 0, label_id, COUNT(*) FROM scoped GROUP BY label_id
             UNION ALL
             SELECT 1, l.group_id, COUNT(DISTINCT s.article_id) FROM scoped s
             JOIN labels l ON l.id = s.label_id WHERE l.group_id IS NOT NULL GROUP BY l.group_id
             UNION ALL
             SELECT 2, 0, COUNT(*) FROM articles a JOIN feeds f ON f.id = a.feed_id
             WHERE (?1 IS NULL OR a.feed_id = ?1) AND (?2 IS NULL OR f.folder_id = ?2){view}
               AND a.id NOT IN (SELECT article_id FROM scoped)"
        );
        let mut out = CategoryCounts::default();
        let mut stmt = self.conn().prepare(&sql)?;
        let rows = stmt.query_map(params![filter.feed_id, filter.folder_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)? as u64))
        })?;
        for r in rows {
            match r? {
                (0, id, n) => out.categories.insert(id, n),
                (1, id, n) => out.groups.insert(id, n),
                (_, _, n) => {
                    out.uncategorized = n;
                    None
                }
            };
        }
        Ok(out)
    }

    /// The articles "find categories with AI" offers next: uncategorized ones
    /// whose feed gave no publisher categories, in `filter`'s scope, not yet
    /// checked, newest first; at most `limit`, with how many are eligible.
    pub fn articles_to_categorize(
        &self,
        filter: ListFilter,
        limit: usize,
    ) -> Result<(Vec<ArticleForCategorizing>, usize)> {
        let q = SearchQuery {
            view: filter.view,
            feed_ids: filter.feed_id.into_iter().collect(),
            folder_ids: filter.folder_id.into_iter().collect(),
            uncategorized: true,
            without_terms: true,
            ..Default::default()
        };
        let (from_where, params, _) = build_where(&q)?;
        let from_where =
            format!("{from_where} AND NOT EXISTS (SELECT 1 FROM category_ai_checks c WHERE c.article_id = a.id)");
        let conn = self.conn();
        let eligible: i64 =
            conn.query_row(&format!("SELECT COUNT(*){from_where}"), rusqlite::params_from_iter(params.iter()), |r| {
                r.get(0)
            })?;
        let mut stmt = conn.prepare(&format!(
            "SELECT a.id, a.title,
                    COALESCE(NULLIF(TRIM(a.summary_text), ''), SUBSTR(COALESCE(a.content_text, ''), 1, {CONTENT_START}))
             {from_where} ORDER BY a.effective_at DESC, a.id DESC LIMIT {limit}"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok(ArticleForCategorizing { id: r.get(0)?, title: r.get(1)?, text: r.get(2)? })
        })?;
        Ok((rows.collect::<rusqlite::Result<_>>()?, eligible as usize))
    }

    /// Saves what "find categories with AI" found, in one transaction: each
    /// name is an existing category (by normalized name) or a new ungrouped
    /// one; the links are confirmed AI links, except that a link the user
    /// rejected stays rejected. Every `checked` article is marked as sent.
    pub fn apply_article_categories(
        &mut self,
        results: &[(ArticleId, Vec<String>)],
        checked: &[ArticleId],
    ) -> Result<ApplyStats> {
        let tx = self.conn_mut().transaction()?;
        let mut stats = ApplyStats::default();
        let t = now();
        for (article, names) in results {
            let mut confirmed = false;
            for name in names {
                if normalize_text(name).is_empty() {
                    continue;
                }
                let (label, created) = crate::research::insert_label(&tx, name)?;
                stats.new_categories += usize::from(created);
                tx.execute(
                    "INSERT INTO article_labels(article_id, label_id, origin, status, suggestion_id, created_at)
                     VALUES (?1, ?2, 'ai', 'confirmed', NULL, ?3)
                     ON CONFLICT(article_id, label_id) DO UPDATE SET status = 'confirmed'
                     WHERE article_labels.status <> 'rejected'",
                    params![article, label, t],
                )?;
                let status: String = tx.query_row(
                    "SELECT status FROM article_labels WHERE article_id = ?1 AND label_id = ?2",
                    params![article, label],
                    |r| r.get(0),
                )?;
                confirmed |= status == "confirmed";
            }
            stats.categorized += usize::from(confirmed);
        }
        for id in checked {
            tx.execute(
                "INSERT INTO category_ai_checks(article_id, checked_at) VALUES (?1, ?2)
                 ON CONFLICT(article_id) DO UPDATE SET checked_at = excluded.checked_at",
                params![id, t],
            )?;
        }
        tx.commit()?;
        Ok(stats)
    }
}

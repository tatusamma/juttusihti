//! Categories, their own entity apart from labels: category groups,
//! ordering, publisher term mapping, an article's manual and AI category,
//! favourites, the category tree and per-category counts.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};

use crate::normalize::normalize_text;
use crate::research::similar_labels;
use crate::search::{SearchQuery, build_where};
use crate::store::{ListFilter, Store};
use crate::{ArticleId, Error, FeedId, Result, View, now};

pub type GroupId = i64;
pub type TermId = i64;
pub type CategoryId = i64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    pub id: CategoryId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub id: GroupId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryNode {
    pub category: Category,
    pub group_id: Option<GroupId>,
    pub term_count: u64,
    /// Titles of the feeds that use this category as their default, by title.
    pub default_for: Vec<String>,
}

/// A feed listed in a group (or among the feeds in no group).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupFeed {
    pub id: FeedId,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupNode {
    pub group: Group,
    pub categories: Vec<CategoryNode>,
    /// Feeds whose articles all belong to the group, by title.
    pub feeds: Vec<GroupFeed>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Favourite {
    Group(GroupId),
    Category(CategoryId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryTree {
    pub groups: Vec<GroupNode>,
    pub ungrouped: Vec<CategoryNode>,
    pub favourites: Vec<Favourite>,
    /// Feeds in no group, by title.
    pub ungrouped_feeds: Vec<GroupFeed>,
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
    pub categories: HashMap<CategoryId, u64>,
    /// Articles per feed that is in a group.
    pub feeds: HashMap<FeedId, u64>,
    /// Articles in no category.
    pub uncategorized: u64,
}

/// A feed's default category, in effect for an article of that feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedDefault {
    pub category: Category,
    pub feed_title: String,
}

/// Where an article's categories come from, only the sources in effect. The
/// manual one, when set, is the article's only category; otherwise
/// `from_terms` (mapped publisher terms); with none, `from_feed` (the feed's
/// default); with neither, `ai`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArticleCategory {
    pub manual: Option<Category>,
    pub ai: Option<Category>,
    /// Categories its publisher terms are mapped to, by name.
    pub from_terms: Vec<Category>,
    pub from_feed: Option<FeedDefault>,
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
    /// Articles given an AI category.
    pub categorized: usize,
    /// Categories that did not exist before.
    pub new_categories: usize,
}

/// A category the AI chose: a name and the existing group it belongs under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiCategory {
    pub name: String,
    pub group: Option<GroupId>,
}

/// An existing category as the AI sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VocabCategory {
    pub name: String,
    pub group_id: Option<GroupId>,
}

/// The groups and categories sent to the AI, in tree order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Vocabulary {
    pub groups: Vec<Group>,
    pub categories: Vec<VocabCategory>,
}

/// Row kinds of the [`Store::category_counts`] statement.
const CATEGORY: i64 = 0;
const GROUP: i64 = 1;
const UNCATEGORIZED: i64 = 2;
const FEED: i64 = 3;

/// Characters of content sent when an article has no description.
const CONTENT_START: usize = 300;

fn clean_name(name: &str, what: &str) -> Result<(String, String)> {
    let name = name.trim().to_string();
    let normalized = normalize_text(&name);
    if normalized.is_empty() {
        return Err(Error::Invalid(format!("{what} name must contain letters or digits")));
    }
    Ok((name, normalized))
}

fn clean_group_name(name: &str) -> Result<(String, String)> {
    clean_name(name, "group")
}

fn clean_category_name(name: &str) -> Result<(String, String)> {
    clean_name(name, "category")
}

/// Creates a category (last in `group` when it exists, else in Ungrouped) or
/// finds the one with the same normalized name, which is never moved. Returns
/// its id and whether it was created.
pub(crate) fn insert_category(conn: &Connection, name: &str, group: Option<GroupId>) -> Result<(CategoryId, bool)> {
    let (name, normalized) = clean_category_name(name)?;
    let group = match group {
        Some(g) => conn
            .query_row("SELECT id FROM category_groups WHERE id = ?1", [g], |r| r.get::<_, GroupId>(0))
            .optional()?,
        None => None,
    };
    let created = conn.execute(
        "INSERT INTO categories(name, normalized, group_id, position, created_at)
         VALUES (?1, ?2, ?3, (SELECT COALESCE(MAX(position) + 1, 0) FROM categories WHERE group_id IS ?3), ?4)
         ON CONFLICT(normalized) DO NOTHING",
        params![name, normalized, group, now()],
    )? > 0;
    let id = conn.query_row("SELECT id FROM categories WHERE normalized = ?1", [normalized], |r| r.get(0))?;
    Ok((id, created))
}

fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation)
}

fn favourite_column(f: Favourite) -> (&'static str, i64) {
    match f {
        Favourite::Group(id) => ("group_id", id),
        Favourite::Category(id) => ("category_id", id),
    }
}

fn category_from_row(r: &rusqlite::Row) -> rusqlite::Result<Category> {
    Ok(Category { id: r.get(0)?, name: r.get(1)? })
}

fn node_from_row(r: &rusqlite::Row) -> rusqlite::Result<CategoryNode> {
    Ok(CategoryNode {
        category: category_from_row(r)?,
        group_id: r.get(2)?,
        term_count: r.get::<_, i64>(3)? as u64,
        default_for: vec![],
    })
}

/// Orders terms by name, then feed.
const BY_NAME: &str = "t.name COLLATE NOCASE, f.title COLLATE NOCASE, t.id";

/// Orders terms by how many articles carry them, most first, then by name.
pub(crate) const BY_ARTICLES: &str = "(SELECT COUNT(*) FROM article_terms x WHERE x.term_id = t.id) DESC,
     t.name COLLATE NOCASE, f.title COLLATE NOCASE, t.id";

fn terms(conn: &Connection, cond: &str, order: &str, category: Option<CategoryId>) -> Result<Vec<TermRow>> {
    let sql = format!(
        "SELECT t.id, t.name, f.title FROM feed_terms t JOIN feeds f ON f.id = t.feed_id
         WHERE {cond} ORDER BY {order}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let map = |r: &rusqlite::Row| Ok(TermRow { id: r.get(0)?, name: r.get(1)?, feed_title: r.get(2)? });
    let rows = match category {
        Some(c) => stmt.query_map([c], map)?.collect::<rusqlite::Result<_>>()?,
        None => stmt.query_map([], map)?.collect::<rusqlite::Result<_>>()?,
    };
    Ok(rows)
}

fn column(conn: &Connection, sql: &str, p: impl rusqlite::Params) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare(sql)?;
    Ok(stmt.query_map(p, |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
}

fn exists(conn: &Connection, sql: &str, id: i64) -> Result<bool> {
    Ok(conn.query_row(sql, [id], |_| Ok(())).optional()?.is_some())
}

impl Store {
    pub fn category_tree(&self) -> Result<CategoryTree> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.name, c.group_id,
                    (SELECT COUNT(*) FROM feed_terms t WHERE t.category_id = c.id)
             FROM categories c ORDER BY c.position, c.name COLLATE NOCASE, c.id",
        )?;
        let nodes: Vec<CategoryNode> = stmt.query_map([], node_from_row)?.collect::<rusqlite::Result<_>>()?;
        let mut stmt =
            conn.prepare("SELECT id, name FROM category_groups ORDER BY position, name COLLATE NOCASE, id")?;
        let mut groups: Vec<GroupNode> = stmt
            .query_map([], |r| {
                Ok(GroupNode { group: Group { id: r.get(0)?, name: r.get(1)? }, categories: vec![], feeds: vec![] })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut ungrouped = Vec::new();
        // Feed titles per default category, by title.
        let mut default_for: HashMap<CategoryId, Vec<String>> = HashMap::new();
        let mut stmt = conn.prepare(
            "SELECT default_category_id, title FROM feeds WHERE default_category_id IS NOT NULL
             ORDER BY title COLLATE NOCASE, id",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))? {
            let (category, title) = row?;
            default_for.entry(category).or_default().push(title);
        }
        for mut n in nodes {
            n.default_for = default_for.remove(&n.category.id).unwrap_or_default();
            match n.group_id.and_then(|g| groups.iter_mut().find(|x| x.group.id == g)) {
                Some(g) => g.categories.push(n),
                None => ungrouped.push(n),
            }
        }
        let mut stmt = conn.prepare("SELECT group_id, category_id FROM favourites ORDER BY position, id")?;
        let favourites = stmt
            .query_map([], |r| {
                Ok(match r.get::<_, Option<GroupId>>(0)? {
                    Some(g) => Favourite::Group(g),
                    None => Favourite::Category(r.get(1)?),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut stmt =
            conn.prepare("SELECT id, title, category_group_id FROM feeds ORDER BY title COLLATE NOCASE, id")?;
        let feeds = stmt
            .query_map([], |r| Ok((GroupFeed { id: r.get(0)?, title: r.get(1)? }, r.get::<_, Option<GroupId>>(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut ungrouped_feeds = Vec::new();
        for (feed, group) in feeds {
            match group.and_then(|g| groups.iter_mut().find(|x| x.group.id == g)) {
                Some(g) => g.feeds.push(feed),
                None => ungrouped_feeds.push(feed),
            }
        }
        Ok(CategoryTree { groups, ungrouped, favourites, ungrouped_feeds })
    }

    /// All groups and categories, in tree order (groups first, then Ungrouped).
    pub fn category_vocabulary(&self) -> Result<Vocabulary> {
        let tree = self.category_tree()?;
        let vocab = |n: &CategoryNode| VocabCategory { name: n.category.name.clone(), group_id: n.group_id };
        let mut categories: Vec<VocabCategory> =
            tree.groups.iter().flat_map(|g| g.categories.iter().map(vocab)).collect();
        categories.extend(tree.ungrouped.iter().map(vocab));
        Ok(Vocabulary { groups: tree.groups.into_iter().map(|g| g.group).collect(), categories })
    }

    // ---- categories --------------------------------------------------------

    /// All categories, by name.
    pub fn categories(&self) -> Result<Vec<Category>> {
        let mut stmt = self.conn().prepare("SELECT id, name FROM categories ORDER BY name COLLATE NOCASE, id")?;
        Ok(stmt.query_map([], category_from_row)?.collect::<rusqlite::Result<_>>()?)
    }

    /// Creates a category, last in Ungrouped, or returns the existing one with
    /// the same normalized name.
    pub fn create_category(&self, name: &str) -> Result<CategoryId> {
        Ok(insert_category(self.conn(), name, None)?.0)
    }

    /// Like `create_category`, and says whether it was created (false: a
    /// category with the same normalized name already existed).
    pub fn create_category_checked(&self, name: &str) -> Result<(CategoryId, bool)> {
        insert_category(self.conn(), name, None)
    }

    pub fn rename_category(&self, id: CategoryId, name: &str) -> Result<()> {
        let (name, normalized) = clean_category_name(name)?;
        match self
            .conn()
            .execute("UPDATE categories SET name = ?1, normalized = ?2 WHERE id = ?3", params![name, normalized, id])
        {
            Ok(0) => Err(Error::NotFound),
            Ok(_) => Ok(()),
            Err(e) if is_unique_violation(&e) => Err(Error::Invalid("a category with that name already exists".into())),
            Err(e) => Err(e.into()),
        }
    }

    /// Deletes a category: its terms return to Unsorted, articles lose it as
    /// their manual or AI category, and a favourite of it vanishes.
    pub fn delete_category(&mut self, id: CategoryId) -> Result<()> {
        let tx = self.conn_mut().transaction()?;
        if tx.execute("DELETE FROM categories WHERE id = ?1", [id])? == 0 {
            return Err(Error::NotFound);
        }
        tx.execute("DELETE FROM article_categories WHERE user_category_id IS NULL AND ai_category_id IS NULL", [])?;
        tx.commit()?;
        Ok(())
    }

    /// Merges `from` into `into`: term mappings and articles' manual and AI
    /// choices move over, `into` keeps its group, and `from` is deleted
    /// (with any favourite of it).
    pub fn merge_categories(&mut self, from: CategoryId, into: CategoryId) -> Result<()> {
        if from == into {
            return Ok(());
        }
        let tx = self.conn_mut().transaction()?;
        let found: i64 =
            tx.query_row("SELECT COUNT(*) FROM categories WHERE id IN (?1, ?2)", params![from, into], |r| r.get(0))?;
        if found != 2 {
            return Err(Error::NotFound);
        }
        tx.execute("UPDATE feed_terms SET category_id = ?2 WHERE category_id = ?1", params![from, into])?;
        tx.execute(
            "UPDATE article_categories SET user_category_id = ?2 WHERE user_category_id = ?1",
            params![from, into],
        )?;
        tx.execute("UPDATE article_categories SET ai_category_id = ?2 WHERE ai_category_id = ?1", params![from, into])?;
        tx.execute("UPDATE feeds SET default_category_id = ?2 WHERE default_category_id = ?1", params![from, into])?;
        tx.execute("DELETE FROM categories WHERE id = ?1", [from])?;
        tx.commit()?;
        Ok(())
    }

    /// Existing categories that a proposed name probably duplicates.
    pub fn similar_categories(&self, name: &str) -> Result<Vec<Category>> {
        let wanted = normalize_text(name);
        let mut stmt =
            self.conn().prepare("SELECT id, name, normalized FROM categories ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| Ok((category_from_row(r)?, r.get::<_, String>(2)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (category, normalized) = row?;
            if similar_labels(&wanted, &normalized) {
                out.push(category);
            }
        }
        Ok(out)
    }

    /// Sets (or with `None` clears) an article's manual category. Clearing
    /// returns the article to its publisher-term and AI categories.
    pub fn set_article_category(&self, article: ArticleId, category: Option<CategoryId>) -> Result<()> {
        let conn = self.conn();
        if !exists(conn, "SELECT 1 FROM articles WHERE id = ?1", article)? {
            return Err(Error::NotFound);
        }
        match category {
            Some(c) => {
                if !exists(conn, "SELECT 1 FROM categories WHERE id = ?1", c)? {
                    return Err(Error::NotFound);
                }
                conn.execute(
                    "INSERT INTO article_categories(article_id, user_category_id, ai_category_id, updated_at)
                     VALUES (?1, ?2, NULL, ?3)
                     ON CONFLICT(article_id) DO UPDATE SET user_category_id = excluded.user_category_id,
                                                           updated_at = excluded.updated_at",
                    params![article, c, now()],
                )?;
            }
            None => {
                conn.execute(
                    "UPDATE article_categories SET user_category_id = NULL, updated_at = ?2 WHERE article_id = ?1",
                    params![article, now()],
                )?;
                conn.execute(
                    "DELETE FROM article_categories
                     WHERE article_id = ?1 AND user_category_id IS NULL AND ai_category_id IS NULL",
                    [article],
                )?;
            }
        }
        Ok(())
    }

    /// Sets (or with `None` clears) the category given to a feed's articles that
    /// have no mapped publisher term.
    pub fn set_feed_default_category(&self, feed: FeedId, category: Option<CategoryId>) -> Result<()> {
        if let Some(c) = category
            && !exists(self.conn(), "SELECT 1 FROM categories WHERE id = ?1", c)?
        {
            return Err(Error::NotFound);
        }
        match self.conn().execute("UPDATE feeds SET default_category_id = ?1 WHERE id = ?2", params![category, feed])? {
            0 => Err(Error::NotFound),
            _ => Ok(()),
        }
    }

    /// Puts a feed in a group (or with `None` takes it out): every article of
    /// the feed then counts as in that group.
    pub fn set_feed_group(&self, feed: FeedId, group: Option<GroupId>) -> Result<()> {
        if let Some(g) = group
            && !exists(self.conn(), "SELECT 1 FROM category_groups WHERE id = ?1", g)?
        {
            return Err(Error::NotFound);
        }
        match self.conn().execute("UPDATE feeds SET category_group_id = ?1 WHERE id = ?2", params![group, feed])? {
            0 => Err(Error::NotFound),
            _ => Ok(()),
        }
    }

    /// An article's categories, only the sources in effect.
    pub fn article_category(&self, article: ArticleId) -> Result<ArticleCategory> {
        let conn = self.conn();
        let one = |col: &str| -> Result<Option<Category>> {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT c.id, c.name FROM article_categories ac JOIN categories c ON c.id = ac.{col}
                         WHERE ac.article_id = ?1"
                    ),
                    [article],
                    category_from_row,
                )
                .optional()?)
        };
        let manual = one("user_category_id")?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT c.id, c.name FROM article_terms t JOIN feed_terms ft ON ft.id = t.term_id
             JOIN categories c ON c.id = ft.category_id WHERE t.article_id = ?1
             ORDER BY c.name COLLATE NOCASE, c.id",
        )?;
        let from_terms: Vec<Category> =
            stmt.query_map([article], category_from_row)?.collect::<rusqlite::Result<_>>()?;
        let from_feed = if manual.is_some() || !from_terms.is_empty() {
            None
        } else {
            conn.query_row(
                "SELECT c.id, c.name, f.title FROM articles a JOIN feeds f ON f.id = a.feed_id
                 JOIN categories c ON c.id = f.default_category_id WHERE a.id = ?1",
                [article],
                |r| Ok(FeedDefault { category: category_from_row(r)?, feed_title: r.get(2)? }),
            )
            .optional()?
        };
        let ai = if from_terms.is_empty() && from_feed.is_none() { one("ai_category_id")? } else { None };
        Ok(ArticleCategory { manual, ai, from_terms, from_feed })
    }

    // ---- groups ------------------------------------------------------------

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

    /// The group with the same normalized name, else a new one, last.
    pub fn find_or_create_group(&self, name: &str) -> Result<GroupId> {
        let (_, normalized) = clean_group_name(name)?;
        match self
            .conn()
            .query_row("SELECT id FROM category_groups WHERE normalized = ?1", [normalized], |r| r.get(0))
            .optional()?
        {
            Some(id) => Ok(id),
            None => self.create_group(name),
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
            tx.query_row("SELECT COALESCE(MAX(position) + 1, 0) FROM categories WHERE group_id IS NULL", [], |r| {
                r.get(0)
            })?;
        let freed = column(
            &tx,
            "SELECT id FROM categories WHERE group_id = ?1 ORDER BY position, name COLLATE NOCASE, id",
            [id],
        )?;
        for (i, category) in freed.iter().enumerate() {
            tx.execute(
                "UPDATE categories SET group_id = NULL, position = ?1 WHERE id = ?2",
                params![next + i as i64, category],
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

    pub fn move_category(&mut self, category: CategoryId, group: Option<GroupId>, index: usize) -> Result<()> {
        let tx = self.conn_mut().transaction()?;
        let old: Option<GroupId> = tx
            .query_row("SELECT group_id FROM categories WHERE id = ?1", [category], |r| r.get(0))
            .optional()?
            .ok_or(Error::NotFound)?;
        if let Some(g) = group
            && !exists(&tx, "SELECT 1 FROM category_groups WHERE id = ?1", g)?
        {
            return Err(Error::NotFound);
        }
        let siblings = |tx: &Connection, g: Option<GroupId>| {
            column(tx, "SELECT id FROM categories WHERE group_id IS ?1 ORDER BY position, name COLLATE NOCASE, id", [g])
        };
        let renumber = |tx: &Connection, ids: &[i64]| -> Result<()> {
            for (pos, id) in ids.iter().enumerate() {
                tx.execute("UPDATE categories SET position = ?1 WHERE id = ?2", params![pos as i64, id])?;
            }
            Ok(())
        };
        if old != group {
            let mut rest = siblings(&tx, old)?;
            rest.retain(|x| *x != category);
            renumber(&tx, &rest)?;
            tx.execute("UPDATE categories SET group_id = ?1 WHERE id = ?2", params![group, category])?;
        }
        let mut ids = siblings(&tx, group)?;
        ids.retain(|x| *x != category);
        ids.insert(index.min(ids.len()), category);
        renumber(&tx, &ids)?;
        tx.commit()?;
        Ok(())
    }

    // ---- publisher terms ---------------------------------------------------

    pub fn category_terms(&self, category: CategoryId) -> Result<Vec<TermRow>> {
        terms(self.conn(), "t.category_id = ?1", BY_NAME, Some(category))
    }

    /// The terms waiting to be sorted, the most frequent first.
    pub fn unsorted_terms(&self) -> Result<Vec<TermRow>> {
        terms(self.conn(), "t.category_id IS NULL AND t.ignored = 0", BY_ARTICLES, None)
    }

    pub fn ignored_terms(&self) -> Result<Vec<TermRow>> {
        terms(self.conn(), "t.ignored = 1", BY_NAME, None)
    }

    /// Maps a term to a category (or unmaps with `None`); clears `ignored`.
    pub fn map_term(&self, term: TermId, category: Option<CategoryId>) -> Result<()> {
        let n = self
            .conn()
            .execute("UPDATE feed_terms SET category_id = ?1, ignored = 0 WHERE id = ?2", params![category, term])?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    /// Ignoring a term clears its mapping.
    pub fn set_term_ignored(&self, term: TermId, ignored: bool) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE feed_terms SET ignored = ?1, category_id = CASE WHEN ?1 = 1 THEN NULL ELSE category_id END
             WHERE id = ?2",
            params![ignored, term],
        )?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    // ---- favourites --------------------------------------------------------

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

    // ---- counts and AI -----------------------------------------------------

    /// Distinct article counts per category, per group and per feed in a
    /// group, restricted like a list. Category membership: the manual
    /// category; otherwise mapped publisher terms; with none, the feed's
    /// default category; with neither, the AI category. A group also holds
    /// every article of its feeds.
    ///
    /// One statement. The memberships are built from the small tables
    /// (`article_terms`, `article_categories`) and the feed index, so the
    /// wide `articles` rows are read only to apply a view (unread, saved).
    pub fn category_counts(&self, filter: ListFilter) -> Result<CategoryCounts> {
        // Feed and folder ids are integers, written into the statement.
        let feed_is = |col: &str| {
            let mut sql = String::new();
            if let Some(feed) = filter.feed_id {
                sql.push_str(&format!(" AND {col} = {feed}"));
            }
            if let Some(folder) = filter.folder_id {
                sql.push_str(&format!(" AND {col} IN (SELECT id FROM feeds WHERE folder_id = {folder})"));
            }
            sql
        };
        let view = match filter.view {
            View::All => "",
            View::Unread => " AND a.is_read = 0",
            View::Saved => " AND a.is_saved = 1",
        };
        let feeds = feed_is("a.feed_id");
        let filtered = !feeds.is_empty() || !view.is_empty();
        // The articles in scope, when the list is narrowed at all.
        let (scope, total) = if filtered {
            (
                format!("scope_ids(id) AS MATERIALIZED (SELECT a.id FROM articles a WHERE 1{feeds}{view}),"),
                "(SELECT COUNT(*) FROM scope_ids)",
            )
        } else {
            (String::new(), "(SELECT COUNT(*) FROM articles)")
        };
        let in_scope =
            |col: &str| if filtered { format!(" AND {col} IN (SELECT id FROM scope_ids)") } else { String::new() };
        let sql = format!(
            "WITH {scope}
             manual(article_id, category_id) AS MATERIALIZED (
               SELECT article_id, user_category_id FROM article_categories WHERE user_category_id IS NOT NULL
             ),
             termed(article_id, category_id) AS MATERIALIZED (
               SELECT DISTINCT t.article_id, ft.category_id FROM feed_terms ft
               JOIN article_terms t ON t.term_id = ft.id
               WHERE ft.category_id IS NOT NULL{term_feeds}
             ),
             defaulted(article_id, category_id) AS MATERIALIZED (
               SELECT a.id, f.default_category_id FROM feeds f JOIN articles a ON a.feed_id = f.id
               WHERE f.default_category_id IS NOT NULL{own_feeds}
             ),
             member(article_id, category_id) AS (
               SELECT article_id, category_id FROM manual
               UNION ALL
               SELECT article_id, category_id FROM termed
               WHERE article_id NOT IN (SELECT article_id FROM manual)
               UNION ALL
               SELECT article_id, category_id FROM defaulted
               WHERE article_id NOT IN (SELECT article_id FROM manual)
                 AND article_id NOT IN (SELECT article_id FROM termed)
               UNION ALL
               SELECT article_id, ai_category_id FROM article_categories
               WHERE user_category_id IS NULL AND ai_category_id IS NOT NULL
                 AND article_id NOT IN (SELECT article_id FROM termed)
                 AND article_id NOT IN (SELECT article_id FROM defaulted)
             ),
             scoped(article_id, category_id) AS MATERIALIZED (
               SELECT article_id, category_id FROM member WHERE 1{scoped}
             ),
             group_feed(article_id, group_id, feed_id) AS MATERIALIZED (
               SELECT a.id, f.category_group_id, f.id FROM feeds f JOIN articles a ON a.feed_id = f.id
               WHERE f.category_group_id IS NOT NULL{own_feeds}{group_scoped}
             ),
             in_group(article_id, group_id) AS (
               SELECT s.article_id, c.group_id FROM scoped s
               JOIN categories c ON c.id = s.category_id WHERE c.group_id IS NOT NULL
               UNION
               SELECT article_id, group_id FROM group_feed
             )
             SELECT {CATEGORY}, category_id, COUNT(*) FROM scoped GROUP BY category_id
             UNION ALL
             SELECT {GROUP}, group_id, COUNT(*) FROM in_group GROUP BY group_id
             UNION ALL
             SELECT {UNCATEGORIZED}, 0, {total} - (SELECT COUNT(DISTINCT article_id) FROM scoped)
             UNION ALL
             SELECT {FEED}, feed_id, COUNT(*) FROM group_feed GROUP BY feed_id",
            term_feeds = feed_is("ft.feed_id"),
            own_feeds = feed_is("f.id"),
            scoped = in_scope("article_id"),
            group_scoped = in_scope("a.id"),
        );
        let mut out = CategoryCounts::default();
        let mut stmt = self.conn().prepare(&sql)?;
        let rows =
            stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?.max(0) as u64)))?;
        for r in rows {
            match r? {
                (CATEGORY, id, n) => out.categories.insert(id, n),
                (GROUP, id, n) => out.groups.insert(id, n),
                (FEED, id, n) => out.feeds.insert(id, n),
                (UNCATEGORIZED, _, n) => {
                    out.uncategorized = n;
                    None
                }
                (kind, ..) => unreachable!("unknown count row kind {kind}"),
            };
        }
        Ok(out)
    }

    /// The articles "find categories with AI" offers next: uncategorized ones
    /// none of whose publisher terms is mapped or unsorted (no terms, or only
    /// ignored ones), in `filter`'s scope, not yet checked, newest first; at
    /// most `limit`, with how many are eligible.
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

    /// Saves what "find categories with AI" found, in one transaction: a name
    /// is an existing category (by normalized name, never moved) or a new one,
    /// last in its group (Ungrouped when it has none or the group is gone), and
    /// becomes the article's AI category. The manual choice is never touched.
    /// Every `checked` article is marked as sent.
    pub fn apply_article_categories(
        &mut self,
        results: &[(ArticleId, Option<AiCategory>)],
        checked: &[ArticleId],
    ) -> Result<ApplyStats> {
        let tx = self.conn_mut().transaction()?;
        let mut stats = ApplyStats::default();
        let t = now();
        for (article, found) in results {
            let Some(found) = found.as_ref().filter(|c| !normalize_text(&c.name).is_empty()) else { continue };
            // An article deleted while the request ran is skipped, so the rest applies.
            let exists: bool =
                tx.query_row("SELECT EXISTS (SELECT 1 FROM articles WHERE id = ?1)", params![article], |r| r.get(0))?;
            if !exists {
                continue;
            }
            let (category, created) = insert_category(&tx, &found.name, found.group)?;
            stats.new_categories += usize::from(created);
            tx.execute(
                "INSERT INTO article_categories(article_id, user_category_id, ai_category_id, updated_at)
                 VALUES (?1, NULL, ?2, ?3)
                 ON CONFLICT(article_id) DO UPDATE SET ai_category_id = excluded.ai_category_id,
                                                       updated_at = excluded.updated_at",
                params![article, category, t],
            )?;
            stats.categorized += 1;
        }
        for id in checked {
            tx.execute(
                "INSERT INTO category_ai_checks(article_id, checked_at)
                 SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM articles WHERE id = ?1)
                 ON CONFLICT(article_id) DO UPDATE SET checked_at = excluded.checked_at",
                params![id, t],
            )?;
        }
        tx.commit()?;
        Ok(stats)
    }
}

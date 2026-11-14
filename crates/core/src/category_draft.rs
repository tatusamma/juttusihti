//! The AI-proposed category plan: a draft stored in `ai_suggestions`, edited
//! by the user and applied in one transaction.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::categories::{BY_ARTICLES, CategoryId, CategoryNode, Group, GroupId, TermId, TermRow, insert_category};
use crate::normalize::normalize_text;
use crate::pricing::Provider;
use crate::research::NewSuggestion;
use crate::store::Store;
use crate::{Error, Result, now};

pub const DRAFT_KIND: &str = "category_plan";
pub const DRAFT_PROMPT_VERSION: &str = "categories-3";

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum GroupRef {
    Existing(GroupId),
    New(u32),
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum CategoryRef {
    Existing(CategoryId),
    New(u32),
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct NewGroupDraft {
    pub temp: u32,
    pub name: String,
    pub by_user: bool,
    pub ticked: bool,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct NewCategoryDraft {
    pub temp: u32,
    pub name: String,
    /// `None`: the category goes to Ungrouped.
    pub group: Option<GroupRef>,
    pub ticked: bool,
}

/// Puts an existing, still ungrouped category into a group.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Placement {
    pub category: CategoryId,
    pub rename: Option<String>,
    pub group: GroupRef,
    pub ticked: bool,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Mapping {
    pub term: TermId,
    pub category: CategoryRef,
    pub ticked: bool,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug, Default)]
pub struct CategoryDraft {
    pub new_groups: Vec<NewGroupDraft>,
    pub new_categories: Vec<NewCategoryDraft>,
    pub placements: Vec<Placement>,
    pub mappings: Vec<Mapping>,
    pub vague: Vec<TermId>,
    /// The AI's grouping put nearly every category into one group, so no
    /// grouping is suggested.
    #[serde(default)]
    pub grouping_skipped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplyReport {
    pub applied: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanInput {
    pub groups: Vec<Group>,
    pub categories: Vec<CategoryNode>,
    pub terms: Vec<TermRow>,
    pub more_terms: usize,
}

impl CategoryDraft {
    /// True when the draft proposes nothing to change.
    pub fn is_empty(&self) -> bool {
        self.new_groups.is_empty()
            && self.new_categories.is_empty()
            && self.placements.is_empty()
            && self.mappings.is_empty()
    }

    /// A new group counts if ticked and something counting goes into it.
    pub fn group_counts(&self, temp: u32) -> bool {
        self.new_groups.iter().any(|g| g.temp == temp && g.ticked)
            && (self.new_categories.iter().any(|c| c.ticked && c.group == Some(GroupRef::New(temp)))
                || self.placements.iter().any(|p| p.ticked && p.group == GroupRef::New(temp)))
    }

    pub fn group_ref_counts(&self, g: GroupRef) -> bool {
        match g {
            GroupRef::Existing(_) => true,
            GroupRef::New(t) => self.group_counts(t),
        }
    }

    pub fn category_counts(&self, temp: u32) -> bool {
        self.new_categories
            .iter()
            .any(|c| c.temp == temp && c.ticked && c.group.is_none_or(|g| self.group_ref_counts(g)))
    }

    pub fn category_ref_counts(&self, c: CategoryRef) -> bool {
        match c {
            CategoryRef::Existing(_) => true,
            CategoryRef::New(t) => self.category_counts(t),
        }
    }
}

fn exists(tx: &Transaction, sql: &str, id: i64) -> Result<bool> {
    Ok(tx.query_row(sql, [id], |_| Ok(())).optional()?.is_some())
}

/// Makes a new group of the draft, or finds the existing one of that name;
/// `None` for a name without letters or digits.
fn make_group(tx: &Transaction, g: Option<&NewGroupDraft>) -> Result<Option<GroupId>> {
    let Some(g) = g else { return Ok(None) };
    let name = g.name.trim();
    let normalized = normalize_text(name);
    if normalized.is_empty() {
        return Ok(None);
    }
    let found: Option<GroupId> =
        tx.query_row("SELECT id FROM category_groups WHERE normalized = ?1", [&normalized], |r| r.get(0)).optional()?;
    if found.is_some() {
        return Ok(found);
    }
    tx.execute(
        "INSERT INTO category_groups(name, normalized, position, created_at)
         VALUES (?1, ?2, (SELECT COALESCE(MAX(position) + 1, 0) FROM category_groups), ?3)",
        params![name, normalized, now()],
    )?;
    Ok(Some(tx.last_insert_rowid()))
}

fn place(tx: &Transaction, category: CategoryId, group: GroupId) -> Result<()> {
    let pos: i64 =
        tx.query_row("SELECT COALESCE(MAX(position) + 1, 0) FROM categories WHERE group_id = ?1", [group], |r| {
            r.get(0)
        })?;
    tx.execute("UPDATE categories SET group_id = ?1, position = ?2 WHERE id = ?3", params![group, pos, category])?;
    Ok(())
}

impl Store {
    pub fn category_plan_input(&self, max_terms: usize) -> Result<PlanInput> {
        let tree = self.category_tree()?;
        let mut groups = Vec::new();
        let mut categories = Vec::new();
        for g in tree.groups {
            groups.push(g.group);
            categories.extend(g.categories);
        }
        categories.extend(tree.ungrouped);
        let conn = self.conn();
        // The most frequent terms first: sorting them helps the most articles.
        let mut stmt = conn.prepare(&format!(
            "SELECT t.id, t.name, f.title FROM feed_terms t JOIN feeds f ON f.id = t.feed_id
             WHERE t.category_id IS NULL AND t.ignored = 0
             ORDER BY {BY_ARTICLES} LIMIT ?1"
        ))?;
        let terms: Vec<TermRow> = stmt
            .query_map([max_terms as i64], |r| Ok(TermRow { id: r.get(0)?, name: r.get(1)?, feed_title: r.get(2)? }))?
            .collect::<rusqlite::Result<_>>()?;
        let total: i64 =
            conn.query_row("SELECT COUNT(*) FROM feed_terms WHERE category_id IS NULL AND ignored = 0", [], |r| {
                r.get(0)
            })?;
        let more_terms = (total as usize).saturating_sub(terms.len());
        Ok(PlanInput { groups, categories, terms, more_terms })
    }

    pub fn save_category_draft(&self, draft: &CategoryDraft, provider: Provider, model: &str) -> Result<i64> {
        let provider = serde_json::to_value(provider)?;
        self.record_suggestion(&NewSuggestion {
            session_id: None,
            kind: DRAFT_KIND.into(),
            payload: serde_json::to_value(draft)?,
            provider: provider.as_str().unwrap_or_default().into(),
            model: model.into(),
            prompt_version: DRAFT_PROMPT_VERSION.into(),
            confidence: None,
            evidence: None,
        })
    }

    pub fn pending_category_draft(&self) -> Result<Option<(i64, CategoryDraft)>> {
        let row: Option<(i64, String)> = self
            .conn()
            .query_row(
                "SELECT id, payload_json FROM ai_suggestions WHERE kind = ?1 AND status = 'suggested'
                 ORDER BY id DESC LIMIT 1",
                [DRAFT_KIND],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((id, json)) = row else { return Ok(None) };
        match serde_json::from_str(&json) {
            Ok(draft) => Ok(Some((id, draft))),
            // Unreadable (e.g. from another version): set it aside rather than break the screen.
            Err(e) => {
                tracing::warn!(id, error = %e, "unreadable category draft, setting it aside");
                self.discard_category_draft(id)?;
                Ok(None)
            }
        }
    }

    pub fn update_category_draft(&self, id: i64, draft: &CategoryDraft) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE ai_suggestions SET payload_json = ?1 WHERE id = ?2 AND kind = ?3 AND status = 'suggested'",
            params![serde_json::to_string(draft)?, id, DRAFT_KIND],
        )?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    pub fn discard_category_draft(&self, id: i64) -> Result<()> {
        let n = self.conn().execute(
            "UPDATE ai_suggestions SET status = 'rejected', decided_at = ?1
             WHERE id = ?2 AND kind = ?3 AND status = 'suggested'",
            params![now(), id, DRAFT_KIND],
        )?;
        if n == 0 { Err(Error::NotFound) } else { Ok(()) }
    }

    pub fn apply_category_draft(&mut self, id: i64, draft: &CategoryDraft) -> Result<ApplyReport> {
        let tx = self.conn_mut().transaction()?;
        // Only a pending draft applies: a second Approve, or one after Cancel, changes nothing.
        let pending = tx
            .query_row(
                "SELECT 1 FROM ai_suggestions WHERE id = ?1 AND kind = ?2 AND status = 'suggested'",
                params![id, DRAFT_KIND],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !pending {
            return Err(Error::NotFound);
        }
        let (mut applied, mut skipped) = (0, 0);
        // New groups are made when their first category lands, so a group whose
        // contents all changed meanwhile is not left behind empty. `None`: unusable.
        let mut groups: HashMap<u32, Option<GroupId>> = HashMap::new();
        let mut cats: HashMap<u32, CategoryId> = HashMap::new();

        // Resolves a group reference, making a new group on first use; None means it is stale.
        let mut resolve_group =
            |tx: &Transaction, r: GroupRef, applied: &mut usize, skipped: &mut usize| -> Result<Option<GroupId>> {
                let t = match r {
                    GroupRef::Existing(g) => {
                        return Ok(exists(tx, "SELECT 1 FROM category_groups WHERE id = ?1", g)?.then_some(g));
                    }
                    GroupRef::New(t) => t,
                };
                if let Some(known) = groups.get(&t) {
                    return Ok(*known);
                }
                let made = make_group(tx, draft.new_groups.iter().find(|g| g.temp == t))?;
                match made {
                    Some(_) => *applied += 1,
                    None => *skipped += 1,
                }
                groups.insert(t, made);
                Ok(made)
            };

        for c in draft.new_categories.iter().filter(|c| c.ticked && c.group.is_none_or(|g| draft.group_ref_counts(g))) {
            let name = c.name.trim();
            let normalized = normalize_text(name);
            if normalized.is_empty() {
                skipped += 1;
                continue;
            }
            // No group: the category goes to Ungrouped.
            let group = match c.group {
                Some(r) => match resolve_group(&tx, r, &mut applied, &mut skipped)? {
                    Some(g) => Some(g),
                    None => {
                        skipped += 1;
                        continue;
                    }
                },
                None => None,
            };
            let (category, _) = insert_category(&tx, name, None)?;
            let grouped: Option<GroupId> =
                tx.query_row("SELECT group_id FROM categories WHERE id = ?1", [category], |r| r.get(0))?;
            if let (None, Some(group)) = (grouped, group) {
                place(&tx, category, group)?;
            }
            cats.insert(c.temp, category);
            applied += 1;
        }

        for p in draft.placements.iter().filter(|p| p.ticked && draft.group_ref_counts(p.group)) {
            // Only a category that is still ungrouped is placed.
            let current: Option<Option<GroupId>> =
                tx.query_row("SELECT group_id FROM categories WHERE id = ?1", [p.category], |r| r.get(0)).optional()?;
            if current != Some(None) {
                skipped += 1;
                continue;
            }
            let Some(group) = resolve_group(&tx, p.group, &mut applied, &mut skipped)? else {
                skipped += 1;
                continue;
            };
            if let Some(name) = &p.rename {
                let normalized = normalize_text(name);
                let taken = tx
                    .query_row(
                        "SELECT 1 FROM categories WHERE normalized = ?1 AND id != ?2",
                        params![normalized, p.category],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if !normalized.is_empty() && !taken {
                    tx.execute(
                        "UPDATE categories SET name = ?1, normalized = ?2 WHERE id = ?3",
                        params![name.trim(), normalized, p.category],
                    )?;
                }
            }
            place(&tx, p.category, group)?;
            applied += 1;
        }

        for m in draft.mappings.iter().filter(|m| m.ticked && draft.category_ref_counts(m.category)) {
            let category = match m.category {
                CategoryRef::Existing(c) => exists(&tx, "SELECT 1 FROM categories WHERE id = ?1", c)?.then_some(c),
                CategoryRef::New(t) => cats.get(&t).copied(),
            };
            let n = match category {
                Some(c) => tx.execute(
                    "UPDATE feed_terms SET category_id = ?1 WHERE id = ?2 AND category_id IS NULL AND ignored = 0",
                    params![c, m.term],
                )?,
                None => 0,
            };
            if n == 0 {
                skipped += 1;
            } else {
                applied += 1;
            }
        }

        tx.execute(
            "UPDATE ai_suggestions SET status = 'confirmed', decided_at = ?1 WHERE id = ?2 AND kind = ?3",
            params![now(), id, DRAFT_KIND],
        )?;
        tx.commit()?;
        Ok(ApplyReport { applied, skipped })
    }
}

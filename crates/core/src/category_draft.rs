//! The AI-proposed category plan: a draft stored in `ai_suggestions`, edited
//! by the user and applied in one transaction.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use crate::categories::{CategoryNode, Group, GroupId, TermId, TermRow};
use crate::normalize::normalize_text;
use crate::pricing::Provider;
use crate::research::NewSuggestion;
use crate::store::Store;
use crate::{Error, LabelId, Result, now};

pub const DRAFT_KIND: &str = "category_plan";
pub const DRAFT_PROMPT_VERSION: &str = "categories-1";

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum GroupRef {
    Existing(GroupId),
    New(u32),
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum CategoryRef {
    Existing(LabelId),
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
    pub group: GroupRef,
    pub ticked: bool,
}

/// Puts an existing, still ungrouped category into a group.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct Placement {
    pub label: LabelId,
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
    /// A new group counts if ticked and something counting goes into it.
    pub fn group_counts(&self, temp: u32) -> bool {
        self.new_groups.iter().any(|g| g.temp == temp && g.ticked)
            && (self.new_categories.iter().any(|c| c.ticked && c.group == GroupRef::New(temp))
                || self.placements.iter().any(|p| p.ticked && p.group == GroupRef::New(temp)))
    }

    pub fn group_ref_counts(&self, g: GroupRef) -> bool {
        match g {
            GroupRef::Existing(_) => true,
            GroupRef::New(t) => self.group_counts(t),
        }
    }

    pub fn category_counts(&self, temp: u32) -> bool {
        self.new_categories.iter().any(|c| c.temp == temp && c.ticked && self.group_ref_counts(c.group))
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

fn place(tx: &Transaction, label: LabelId, group: GroupId) -> Result<()> {
    let pos: i64 =
        tx.query_row("SELECT COALESCE(MAX(position) + 1, 0) FROM labels WHERE group_id = ?1", [group], |r| r.get(0))?;
    tx.execute("UPDATE labels SET group_id = ?1, position = ?2 WHERE id = ?3", params![group, pos, label])?;
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
        let mut stmt = conn.prepare(
            "SELECT t.id, t.name, f.title FROM feed_terms t JOIN feeds f ON f.id = t.feed_id
             WHERE t.label_id IS NULL AND t.ignored = 0
             ORDER BY t.first_seen_at, t.id LIMIT ?1",
        )?;
        let terms: Vec<TermRow> = stmt
            .query_map([max_terms as i64], |r| Ok(TermRow { id: r.get(0)?, name: r.get(1)?, feed_title: r.get(2)? }))?
            .collect::<rusqlite::Result<_>>()?;
        let total: i64 =
            conn.query_row("SELECT COUNT(*) FROM feed_terms WHERE label_id IS NULL AND ignored = 0", [], |r| r.get(0))?;
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
        row.map(|(id, json)| Ok((id, serde_json::from_str(&json)?))).transpose()
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
        let mut groups: HashMap<u32, GroupId> = HashMap::new();
        let mut cats: HashMap<u32, LabelId> = HashMap::new();

        for g in draft.new_groups.iter().filter(|g| draft.group_counts(g.temp)) {
            let name = g.name.trim();
            let normalized = normalize_text(name);
            if normalized.is_empty() {
                skipped += 1;
                continue;
            }
            let found: Option<GroupId> = tx
                .query_row("SELECT id FROM category_groups WHERE normalized = ?1", [&normalized], |r| r.get(0))
                .optional()?;
            let gid = match found {
                Some(gid) => gid,
                None => {
                    tx.execute(
                        "INSERT INTO category_groups(name, normalized, position, created_at)
                         VALUES (?1, ?2, (SELECT COALESCE(MAX(position) + 1, 0) FROM category_groups), ?3)",
                        params![name, normalized, now()],
                    )?;
                    tx.last_insert_rowid()
                }
            };
            groups.insert(g.temp, gid);
            applied += 1;
        }

        // Resolves a group reference; None means it is stale.
        let resolve_group = |tx: &Transaction, r: GroupRef| -> Result<Option<GroupId>> {
            Ok(match r {
                GroupRef::Existing(g) => exists(tx, "SELECT 1 FROM category_groups WHERE id = ?1", g)?.then_some(g),
                GroupRef::New(t) => groups.get(&t).copied(),
            })
        };

        for c in draft.new_categories.iter().filter(|c| c.ticked && draft.group_ref_counts(c.group)) {
            let name = c.name.trim();
            let normalized = normalize_text(name);
            let group = resolve_group(&tx, c.group)?;
            let (Some(group), false) = (group, normalized.is_empty()) else {
                skipped += 1;
                continue;
            };
            tx.execute(
                "INSERT INTO labels(name, normalized, created_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(normalized) DO NOTHING",
                params![name, normalized, now()],
            )?;
            let (label, grouped): (LabelId, Option<GroupId>) =
                tx.query_row("SELECT id, group_id FROM labels WHERE normalized = ?1", [&normalized], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            if grouped.is_none() {
                place(&tx, label, group)?;
            }
            cats.insert(c.temp, label);
            applied += 1;
        }

        for p in draft.placements.iter().filter(|p| p.ticked && draft.group_ref_counts(p.group)) {
            let group = resolve_group(&tx, p.group)?;
            let current: Option<Option<GroupId>> =
                tx.query_row("SELECT group_id FROM labels WHERE id = ?1", [p.label], |r| r.get(0)).optional()?;
            let (Some(group), Some(None)) = (group, current) else {
                skipped += 1;
                continue;
            };
            if let Some(name) = &p.rename {
                let normalized = normalize_text(name);
                let taken = tx
                    .query_row(
                        "SELECT 1 FROM labels WHERE normalized = ?1 AND id != ?2",
                        params![normalized, p.label],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if !normalized.is_empty() && !taken {
                    tx.execute(
                        "UPDATE labels SET name = ?1, normalized = ?2 WHERE id = ?3",
                        params![name.trim(), normalized, p.label],
                    )?;
                }
            }
            place(&tx, p.label, group)?;
            applied += 1;
        }

        for m in draft.mappings.iter().filter(|m| m.ticked && draft.category_ref_counts(m.category)) {
            let label = match m.category {
                CategoryRef::Existing(l) => exists(&tx, "SELECT 1 FROM labels WHERE id = ?1", l)?.then_some(l),
                CategoryRef::New(t) => cats.get(&t).copied(),
            };
            let n = match label {
                Some(l) => tx.execute(
                    "UPDATE feed_terms SET label_id = ?1 WHERE id = ?2 AND label_id IS NULL AND ignored = 0",
                    params![l, m.term],
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

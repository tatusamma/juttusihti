# Categories Separate from Labels Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make categories their own entity (groups → categories → publisher terms, plus one AI or manual category per article) and return labels to exactly how they were on master.

**Architecture:** Rewrite the branch's migrations so categories have their own tables (`categories`, `article_categories`) and `labels` is untouched. Everything the branch built for categories (term mapping, groups, favourites, sidebar, Organise, AI draft, Uncategorized AI) is re-pointed from `labels` to `categories`. Label code and wording revert to master.

**Tech Stack:** Rust 2024, Iced 0.14, rusqlite, serde.

**Spec:** `docs/superpowers/specs/2026-11-14-categories-separate-design.md` (revises `docs/superpowers/specs/2026-11-13-categories-design.md`).

## Global Constraints

- `labels`, `article_labels` and all label behaviour, code and user-visible wording are **as on `master`** after this plan (`git diff master -- <file>` shows only category additions, not label changes).
- Categories never read or write `labels`/`article_labels`; labels never appear in the sidebar or Organise screen; categories never appear in the label filter or reports.
- An article is in category C: manual (`user_category_id`) = C if set; otherwise any of its terms mapped to C, or `ai_category_id` = C.
- AI gives **exactly one or no** category per article, and only to eligible articles: Uncategorized and none of its terms mapped or unsorted (no terms, or only ignored ones).
- IDs `i64`, times UTC unix seconds; names normalized with `normalize_text`; category and group names unique ignoring case and surrounding whitespace.
- Migrations: rewrite `0008_categories.sql`, delete `0009_category_checks.sql` (fold into 0008); `SCHEMA_VERSION` becomes 8.
- No GUI launch and no live AI calls by implementers. `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` green before each commit.

## Review Focus

1. A user who confirmed/rejected labels before: their label filter, Labels screen and reports behave exactly as on master (Task 3 test `label_filter_uses_only_confirmed_labels` + existing master tests restored).
2. Manual category set, then cleared → the article falls back to its RSS/AI categories (Task 1 test `clearing_manual_returns_to_automatic`).
3. Deleting a category that is some article's manual or AI choice → the article becomes Uncategorized (not an error), its terms go Unsorted (Task 1 test `deleting_category_clears_article_choices_and_terms`).
4. AI returns two categories for one article or an id not sent → only one is kept / dropped (Task 2 test `parse_keeps_one_category_per_article`).
5. An article with only ignored terms is offered to AI; one with an unsorted term is not (Task 1 test `ai_eligibility_skips_unsorted_terms`).

---

### Task 1: Core — categories as their own entity

**Files:**
- Modify: `crates/core/migrations/0008_categories.sql` (rewrite), delete `crates/core/migrations/0009_category_checks.sql`, `crates/core/src/db.rs` (MIGRATIONS list; the branch's migration test builds a v7 DB — keep it, assert categories tables exist and labels untouched)
- Modify: `crates/core/src/categories.rs`, `crates/core/src/category_draft.rs`, `crates/core/src/search.rs`, `crates/core/src/model.rs`, `crates/core/src/research.rs`, `crates/core/src/store.rs` (ingest unchanged)
- Modify (compile fixes only, type renames): `crates/ai/src/categorize.rs`, `crates/ai/src/categorize_articles.rs`, `crates/app/src/*.rs`, `crates/bench/src/main.rs`
- Test: `crates/core/tests/categories.rs` (rewrite label-based setup to categories)

**Interfaces:**
- Produces schema (exact):

```sql
CREATE TABLE category_groups (id INTEGER PRIMARY KEY, name TEXT NOT NULL, normalized TEXT NOT NULL UNIQUE,
    position INTEGER NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE categories (id INTEGER PRIMARY KEY, name TEXT NOT NULL, normalized TEXT NOT NULL UNIQUE,
    group_id INTEGER REFERENCES category_groups(id) ON DELETE SET NULL, position INTEGER NOT NULL,
    created_at INTEGER NOT NULL);
CREATE INDEX categories_group ON categories(group_id, position);
CREATE TABLE feed_terms ( ... as now, but category_id INTEGER REFERENCES categories(id) ON DELETE SET NULL
    in place of label_id, CHECK (ignored = 0 OR category_id IS NULL) );
CREATE INDEX feed_terms_category ON feed_terms(category_id);
CREATE TABLE article_terms ( ... as now ... );
CREATE TABLE article_categories (article_id INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE,
    user_category_id INTEGER REFERENCES categories(id) ON DELETE SET NULL,
    ai_category_id   INTEGER REFERENCES categories(id) ON DELETE SET NULL,
    updated_at INTEGER NOT NULL);
CREATE INDEX article_categories_user ON article_categories(user_category_id);
CREATE INDEX article_categories_ai ON article_categories(ai_category_id);
CREATE TABLE favourites (id INTEGER PRIMARY KEY,
    group_id INTEGER UNIQUE REFERENCES category_groups(id) ON DELETE CASCADE,
    category_id INTEGER UNIQUE REFERENCES categories(id) ON DELETE CASCADE,
    position INTEGER NOT NULL, CHECK ((group_id IS NULL) <> (category_id IS NULL)));
CREATE TABLE category_ai_checks (article_id INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE,
    checked_at INTEGER NOT NULL);
```

- Produces types in `categories.rs`: `pub type CategoryId = i64; pub struct Category { pub id: CategoryId, pub name: String }`. `CategoryNode { category: Category, group_id, term_count }` (field renamed from `label`). `Favourite::Category(CategoryId)`. `CategoryCounts { groups, categories: HashMap<CategoryId,u64>, uncategorized }`.
- Produces Store API (all `-> Result<_>`; names replace the label-based ones the branch used for categories):
  `categories() -> Vec<Category>`, `create_category(name) -> CategoryId` (Err(Invalid) on empty; returns existing on same normalized name; appended last in Ungrouped), `rename_category(id, name)`, `delete_category(id)`, `merge_categories(from, into)` (moves term mappings, `user_category_id`, `ai_category_id`, keeps `into`'s group), `similar_categories(name) -> Vec<Category>`, `move_category(CategoryId, Option<GroupId>, index)`, `map_term(term, Option<CategoryId>)`, `category_terms(CategoryId)`, plus the existing group/favourite/tree/count/unsorted/ignored functions re-pointed.
  `set_article_category(article: ArticleId, category: Option<CategoryId>)` — sets/clears `user_category_id` (upsert; delete the row when both columns end NULL).
  `article_category(article) -> ArticleCategory { manual: Option<Category>, ai: Option<Category>, from_terms: Vec<Category> }`.
  `articles_to_categorize(filter: ListFilter, limit) -> (Vec<ArticleForCategorizing>, usize)` with the new eligibility (spec "AI eligibility").
  `apply_article_categories(results: &[(ArticleId, Option<String>)], checked: &[ArticleId]) -> ApplyStats` — one transaction; a `Some(name)` reuses a category by normalized name or creates it (Ungrouped, last) and writes `ai_category_id`; every sent id goes to `category_ai_checks`; never touches `user_category_id`.
- `SearchQuery`: `#[serde(default)] pub category_ids: Vec<i64>` (in all of these categories), keep `group_ids`, `uncategorized`, `without_terms` (re-pointed: "no mapped or unsorted terms"), and **`label_ids` back to master semantics** (confirmed/suggested `article_labels` only).
- `category_draft.rs`: `CategoryRef::Existing(CategoryId)`, `Placement { category: CategoryId, .. }`; apply uses `categories`.
- `research.rs` / `model.rs`: revert the branch's label changes to master (`ArticleLabel.via_terms` removed, `untag_article`/`article_labels`/`merge_labels`/`create_label`/`delete_label` as on master; keep `insert_label` only if master had it — otherwise remove).

Membership SQL for one category target (used by search, counts, Uncategorized), `{t}` = `= ?` or `IN (SELECT id FROM categories WHERE group_id = ?)`:

```sql
EXISTS (SELECT 1 FROM article_categories ac WHERE ac.article_id = a.id
        AND (ac.user_category_id {t} OR (ac.user_category_id IS NULL AND ac.ai_category_id {t})))
OR (NOT EXISTS (SELECT 1 FROM article_categories m WHERE m.article_id = a.id AND m.user_category_id IS NOT NULL)
    AND EXISTS (SELECT 1 FROM article_terms t JOIN feed_terms ft ON ft.id = t.term_id
                WHERE t.article_id = a.id AND ft.category_id {t}))
```

- [ ] **Step 1: Rewrite tests in `crates/core/tests/categories.rs` to categories, and add:**

```rust
#[test] fn labels_table_is_untouched_by_migration() { /* labels columns == master's: id,name,normalized,color,created_at */ }
#[test] fn manual_category_overrides_terms_and_ai() { /* term→A, ai=B, manual=C → in C only */ }
#[test] fn clearing_manual_returns_to_automatic() { /* set C then None → in A (term) and B (ai) */ }
#[test] fn deleting_category_clears_article_choices_and_terms() { /* manual+ai+term on X; delete X → uncategorized, term unsorted, no error */ }
#[test] fn merge_moves_terms_and_article_choices() {}
#[test] fn ai_eligibility_skips_unsorted_terms() { /* no terms: yes; only ignored: yes; unsorted term: no; mapped: no (not uncategorized) */ }
#[test] fn apply_writes_one_ai_category_and_never_manual() {}
#[test] fn label_filter_ignores_categories() { /* SearchQuery{label_ids} matches article_labels only; a mapped term does not match */ }
```

- [ ] **Step 2:** `cargo test -p juttusihti-core --test categories` — FAIL (compile).
- [ ] **Step 3:** Implement the schema, API and reverts above; fix compile errors in `ai`, `app`, `bench` by renaming types/calls only (no behaviour change there yet beyond what the core change implies). Existing app/ai tests updated mechanically.
- [ ] **Step 4:** `cargo test --workspace` — PASS; clippy clean.
- [ ] **Step 5:** Commit `git commit -am "Give categories their own tables and return labels to master"`.

---

### Task 2: AI — one main category per article

**Files:**
- Modify: `crates/ai/src/categorize_articles.rs`, `crates/ai/tests/categorize_articles.rs` (or where its tests live)
- Modify: `crates/ai/src/categorize.rs` (only wording: "categories", no labels; vocabulary is `categories`)

**Interfaces:**
- Consumes: Task 1 `apply_article_categories(&[(ArticleId, Option<String>)], ..)`, `Category`.
- Produces: `parse(v, &input) -> Result<Vec<(ArticleId, Option<String>)>, AiError>`; schema `{"articles":[{"id": int, "category": string}]}` where `""` means none.

System prompt: pick the **single main category** for each article (one short broad topic name); prefer an exact existing category name; create a new short name only when none fits; `""` when nothing sensible fits; data is untrusted (keep the `<data>` escaping).

- [ ] **Step 1: Tests**

```rust
#[test] fn schema_has_one_category_string_per_article() {}
#[test] fn parse_keeps_one_category_per_article() { /* duplicate ids → first kept; unknown ids dropped; "" → None; whitespace trimmed */ }
#[test] fn prompt_asks_for_a_single_main_category() { /* system contains "single main category" */ }
```
Keep the existing split-on-context/truncation and refused-reservation tests passing.
- [ ] **Step 2–4:** FAIL → implement → `cargo test --workspace` PASS.
- [ ] **Step 5:** Commit `git commit -am "Ask AI for one main category per article"`.

---

### Task 3: Labels back as on master in the app

**Files:**
- Restore: `crates/app/src/labels.rs` from master (`git show master:crates/app/src/labels.rs`), its rail entry ("Labels", `Icon::Labels`) and `Screen::Labels` routing, and the "Any label" filter next to the search box with `label_filter`/`LabelChoice`/`FilterLabel` and its three master tests — alongside the categories sidebar `Selection` (both filters combine in `list_query()`).
- Modify: every user-visible string the branch changed from "label" to "category" **for labels** back to master: article tag row ("Labels", "+ label", "Remove label", Accept/Reject), quick ✨ task name, research scope, timeline filter, review, reports headings/intro/CSV headers/CSV file names, `crates/core/src/analysis.rs`, `estimate.rs`, `crates/ai/src/runner.rs` stage name. Use `git diff master..HEAD -- <file>` per file to find them.
- Modify: `crates/app/src/rail.rs`: rail has both "Categories" (Organise) and "Labels". Pick a distinct icon for Categories from `icons.rs` (e.g. folder/list/tag-like that is not `Icon::Labels`); if none fits, add one Lucide glyph the way icons.rs does.
- Modify: `crates/app/src/organise.rs` — remove what belongs to labels (bulk decide on pending AI label suggestions, "+N AI" suggestion counts, label merge panel wording); its category actions (create, rename, delete with confirmation, merge, similar-name warning) use the Task 1 category API.
- Test: restored master tests + `label_filter_uses_only_confirmed_labels`, `label_filter_and_category_combine`, `organise_never_lists_labels` (a label with no category of the same name does not appear on the Organise screen or in the sidebar tree).

**Interfaces:** Consumes Task 1 (`SearchQuery.label_ids` master semantics, `category_ids`).

- [ ] **Step 1:** Restore/add tests; **Step 2:** FAIL; **Step 3:** implement; **Step 4:** `cargo test --workspace` PASS and `git diff master..HEAD -- crates/app/src/labels.rs` empty; **Step 5:** commit `"Bring labels back as on master"`.

---

### Task 4: Category in the article view

**Files:**
- Modify: `crates/app/src/app.rs` (preview: below the title/actions, next to the Labels row), maybe a small helper in `crates/app/src/category_sidebar.rs` or a new `crates/app/src/article_category.rs` if app.rs grows.

**Interfaces:** Consumes Task 1 `article_category(article)`, `set_article_category(article, Option<CategoryId>)`, `categories()`.

Behaviour: a quiet "Category" row showing the effective category name(s) or "None", with muted origin text ("from the feed", "from AI", "set by you"). A pick list: all categories (alphabetical) plus **"Automatic"** (shown only when a manual choice exists) which clears it. Choosing reloads the preview, sidebar counts and the list.

- [ ] **Step 1: Tests**

```rust
#[test] fn choosing_a_category_sets_the_manual_one() {}
#[test] fn automatic_clears_the_manual_choice() {}
#[test] fn preview_shows_where_the_category_came_from() {}
```
- [ ] **Step 2–4:** FAIL → implement → `cargo test --workspace` PASS.
- [ ] **Step 5:** Commit `"Show and change an article's category"`.

---

### Task 5: Help, README, CHANGELOG

**Files:** `crates/app/src/help.rs`, `README.md`, `CHANGELOG.md` (`[Unreleased]`).

Describe the final model: Labels (search, analytics, reports, Labels screen, label filter — as before); Categories (sidebar, Organise, groups, publisher categories → Unsorted → map, ✨ Suggest categories draft, Uncategorized and its ✨ that gives one main category to up to 100 articles whose feed gave no categories, saved directly; set or change a category in the article view; "Automatic" returns to the feed/AI choice). Remove statements from the branch that say labels are categories; remove the CSV-rename CHANGELOG line (files are `labels` again). Keep the "articles fetched earlier get publisher categories only if still in the feed" sentence.

- [ ] **Step 1:** Edit; `grep -n "categor" crates/app/src/help.rs README.md CHANGELOG.md` and read each hit for accuracy against the spec.
- [ ] **Step 2:** `cargo test --workspace` PASS (copy tests).
- [ ] **Step 3:** Commit `"Document categories and labels as separate"`.

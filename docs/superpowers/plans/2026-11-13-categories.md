# Categories Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Groups → categories (today's labels) → publisher terms, organised by hand or with an editable AI draft, browsed from a hideable reader sidebar with favourites, search and drag to reorder.

**Architecture:** Publisher terms are parsed from feeds and stored per feed in SQLite (`feed_terms`, `article_terms`). Labels gain a `group_id` and `position`; groups and favourites are new tables. Category membership is resolved at query time in `search.rs` (confirmed label row OR mapped term, unless rejected). The AI step produces a `CategoryDraft` stored in `ai_suggestions`; Approve applies it in one transaction. UI: a reusable drag-and-drop helper, a new Organise screen (replaces `labels.rs`) and a reader sidebar.

**Tech Stack:** Rust 2024, Iced 0.14, rusqlite, feed-rs 3, serde_json, tokio.

**Spec:** `docs/superpowers/specs/2026-11-13-categories-design.md`

## Global Constraints

- SQLite is the only store; one new migration `crates/core/migrations/0008_categories.sql`, registered in `crates/core/src/db.rs` `MIGRATIONS`.
- IDs are `INTEGER` (`i64`), times are UTC unix seconds (`crate::now()`).
- Database tables and Rust names stay `labels` / `Label` / `LabelId`; only user-visible text says "category"/"categories".
- Names are normalized with `crate::normalize::normalize_text` (keeps ä/ö/å distinct).
- Group names unique ignoring case and surrounding whitespace (`normalized` column UNIQUE).
- Refresh never calls AI. AI runs only after the user consents to a shown estimate, within `AiDefaults` limits.
- What AI receives: term texts with feed titles, category names, group names. Never article text.
- No DuckDB schema changes; reports change wording only.
- Style: existing `style.rs` tokens (warm paper, amber accent, light and dark), Lucide icons via `icons.rs`.
- Run `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` before each commit.

## Review Focus

1. A feed that sends the same term twice on one item, or with different case/whitespace ("AI", " ai ") → one term, one link (test in Task 1 and Task 2).
2. Removing a term-derived category from an article, then refreshing the feed → it stays removed (Task 3 test `rejection_survives_refresh`).
3. Deleting a category whose terms are mapped → terms reappear in Unsorted, nothing else is lost; deleting a group → its categories become Ungrouped and favourites of the group vanish (Task 4 tests).
4. An AI draft approved after the user mapped one of its terms by hand, deleted one of its target groups, or renamed a category to a colliding name → those items are skipped and counted, the rest applies (Task 5 test `apply_skips_stale_items`).
5. AI returns unknown IDs, an existing grouped category in `place_categories`, or a duplicate new group name → dropped or merged, never an error that loses the whole draft (Task 6 test `parse_drops_unknown_and_grouped`).

---

### Task 1: Parse publisher terms from feeds

**Files:**
- Modify: `crates/core/src/model.rs` (add `NewTerm`, field on `NewArticle`)
- Modify: `crates/core/src/feed.rs` (extract in `parse`)
- Modify: every `NewArticle { .. }` literal (24 sites, `grep -rn "NewArticle {" crates`) to add `terms: vec![]`

**Interfaces:**
- Produces: `pub struct NewTerm { pub name: String, pub scheme: String }` (derive `Debug, Clone, PartialEq, Eq`); `NewArticle.terms: Vec<NewTerm>`; constants `MAX_TERM_CHARS: usize = 100`, `MAX_TERMS_PER_ENTRY: usize = 20` in `feed.rs`.

- [ ] **Step 1: Write failing tests in `feed.rs` `mod tests`**

```rust
#[test]
fn rss_item_categories_become_terms() {
    let rss = r#"<rss version="2.0"><channel><title>T</title><category>Channel</category>
      <item><title>A</title><guid>1</guid><category>Politiikka</category>
      <category domain="http://x/tax">AI</category><category> ai </category><category>  </category></item>
    </channel></rss>"#;
    let f = parse(rss.as_bytes(), None).unwrap();
    assert_eq!(f.entries[0].terms, vec![
        NewTerm { name: "Politiikka".into(), scheme: "".into() },
        NewTerm { name: "AI".into(), scheme: "http://x/tax".into() },
        NewTerm { name: "ai".into(), scheme: "".into() },
    ]);  // channel category is NOT attached; blank dropped
}

#[test]
fn atom_label_wins_over_term() {
    // <category term="tech" scheme="urn:s" label="Technology"/> and <category term="Space"/>
    // → [NewTerm{"Technology","urn:s"}, NewTerm{"Space",""}]
}

#[test]
fn json_feed_tags_become_terms() {
    // item "tags": ["Sää", "Sää"] → [NewTerm{"Sää",""}]  (duplicate by normalized name+scheme dropped)
}

#[test]
fn terms_are_bounded() {
    // 30 categories, one 300 chars long → 20 terms, long one cut to 100 chars on a char boundary
}
```

- [ ] **Step 2: Run** `cargo test -p juttusihti-core feed::tests` — expect compile failure (`NewTerm` missing).

- [ ] **Step 3: Implement**

In `parse`, from `e.categories` (feed-rs `Category { term, scheme, label }`): name = `label` if non-blank else `term`, whitespace collapsed and trimmed, cut to `MAX_TERM_CHARS`; scheme = `scheme` trimmed, lowercased, `""` when absent; skip when `normalize_text(name)` is empty; dedupe by `(normalize_text(name), scheme)` keeping first; take `MAX_TERMS_PER_ENTRY`. Feed-level `feed.categories` are ignored. Add `terms: vec![]` to all other `NewArticle` literals.

- [ ] **Step 4: Run** `cargo test -p juttusihti-core` — PASS.

- [ ] **Step 5: Commit** `git commit -am "Parse publisher terms from feed entries"`

---

### Task 2: Schema and storing terms on refresh

**Files:**
- Create: `crates/core/migrations/0008_categories.sql`
- Modify: `crates/core/src/db.rs` (register migration)
- Modify: `crates/core/src/store.rs` `ingest`
- Test: `crates/core/tests/categories.rs` (new)

**Interfaces:**
- Produces tables (exact):

```sql
CREATE TABLE category_groups (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    normalized TEXT NOT NULL UNIQUE,
    position   INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
ALTER TABLE labels ADD COLUMN group_id INTEGER REFERENCES category_groups(id) ON DELETE SET NULL;
ALTER TABLE labels ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
CREATE INDEX labels_group ON labels(group_id, position);

-- Publisher category words, per feed. Unsorted = label_id IS NULL AND ignored = 0.
CREATE TABLE feed_terms (
    id            INTEGER PRIMARY KEY,
    feed_id       INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    normalized    TEXT NOT NULL,
    scheme        TEXT NOT NULL DEFAULT '',
    label_id      INTEGER REFERENCES labels(id) ON DELETE SET NULL,
    ignored       INTEGER NOT NULL DEFAULT 0 CHECK (ignored IN (0, 1)),
    first_seen_at INTEGER NOT NULL,
    CHECK (ignored = 0 OR label_id IS NULL),
    UNIQUE (feed_id, normalized, scheme)
);
CREATE INDEX feed_terms_label ON feed_terms(label_id);

CREATE TABLE article_terms (
    article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    term_id    INTEGER NOT NULL REFERENCES feed_terms(id) ON DELETE CASCADE,
    PRIMARY KEY (article_id, term_id)
) WITHOUT ROWID;
CREATE INDEX article_terms_term ON article_terms(term_id, article_id);

CREATE TABLE favourites (
    id       INTEGER PRIMARY KEY,
    group_id INTEGER UNIQUE REFERENCES category_groups(id) ON DELETE CASCADE,
    label_id INTEGER UNIQUE REFERENCES labels(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    CHECK ((group_id IS NULL) <> (label_id IS NULL))
);
```

- [ ] **Step 1: Write failing tests in `crates/core/tests/categories.rs`**

```rust
#[test]
fn ingest_links_terms_idempotently() {
    // store with one feed; ingest entry with terms [AI, Politiikka] twice (second time unchanged content)
    // → count(feed_terms)=2, count(article_terms)=2
}
#[test]
fn same_word_in_two_feeds_is_two_terms() { /* "AI" in feed 1 and feed 2 → 2 rows */ }
#[test]
fn unchanged_article_gains_terms_on_refresh() {
    // ingest entry without terms, then same entry (same content hash) with terms → links created
}
#[test]
fn new_terms_start_unsorted() { /* label_id IS NULL AND ignored = 0 */ }
#[test]
fn removing_feed_removes_its_terms() { /* remove_feed → feed_terms and article_terms empty */ }
```

- [ ] **Step 2: Run** `cargo test -p juttusihti-core --test categories` — FAIL (no tables).

- [ ] **Step 3: Implement** the migration and in `ingest`, for every entry that has an article id (inserted, updated or unchanged; not the `same_story` skip), with cached statements: `INSERT INTO feed_terms(feed_id, name, normalized, scheme, first_seen_at) VALUES (…) ON CONFLICT(feed_id, normalized, scheme) DO NOTHING`, then select its id, then `INSERT OR IGNORE INTO article_terms`. Same transaction as the article write. Update the `db.rs` schema-version test if it asserts a number.

- [ ] **Step 4: Run** `cargo test -p juttusihti-core` — PASS.

- [ ] **Step 5: Commit** `git commit -am "Store publisher terms per feed on refresh"`

---

### Task 3: Category membership in filters and on articles

**Files:**
- Modify: `crates/core/src/search.rs` (`SearchQuery`, `build_where`)
- Modify: `crates/core/src/model.rs` (`ArticleLabel.via_terms`)
- Modify: `crates/core/src/research.rs` (`article_labels`, `untag_article`, `merge_labels`)
- Test: `crates/core/tests/categories.rs`

**Interfaces:**
- Produces: `SearchQuery.group_ids: Vec<i64>` with `#[serde(default)]` — articles must be in at least one category of each listed group. `label_ids` keeps its meaning ("in all of these categories") but now includes term-derived membership. `ArticleLabel { label, origin, status, via_terms: bool }`.

Membership of article `a` in category `L` (the rule all later tasks rely on):

```sql
EXISTS (SELECT 1 FROM article_labels x WHERE x.article_id = a.id AND x.label_id = L AND x.status IN {statuses})
OR EXISTS (SELECT 1 FROM article_terms t JOIN feed_terms ft ON ft.id = t.term_id
           WHERE t.article_id = a.id AND ft.label_id = L
             AND NOT EXISTS (SELECT 1 FROM article_labels r WHERE r.article_id = a.id
                             AND r.label_id = ft.label_id AND r.status = 'rejected'))
```

For a group, replace `= L` with `IN (SELECT id FROM labels WHERE group_id = G)` in both branches. `include_suggested` affects only the first branch.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn mapped_term_puts_article_in_category() { /* map term AI→label "Tekoäly"; search label_ids=[L] → 1 item */ }
#[test]
fn group_filter_returns_each_article_once() {
    // article has terms mapped to two categories of the same group, plus a confirmed label in it
    // search group_ids=[G] → total 1, items.len() 1
}
#[test]
fn rejection_beats_mapping() { /* untag_article(a, L) on a term-derived category → not in label filter */ }
#[test]
fn rejection_survives_refresh() { /* reject, ingest same entry again → still excluded */ }
#[test]
fn article_labels_shows_term_derived() {
    // returns ArticleLabel{via_terms:true, origin:User, status:Confirmed}; omitted after rejection
}
#[test]
fn merge_moves_term_mappings() { /* merge_labels(from, into) → feed_terms.label_id = into; into keeps group_id */ }
#[test]
fn delete_category_returns_terms_to_unsorted() { /* delete_label → feed_terms.label_id NULL, ignored 0 */ }
```

- [ ] **Step 2: Run** `cargo test -p juttusihti-core --test categories` — FAIL.

- [ ] **Step 3: Implement**
  - `build_where`: replace the `label_ids` loop body with the membership rule; add the `group_ids` loop.
  - `article_labels(article)`: existing rows (unchanged) plus, for each label reached through the article's mapped terms that has **no** `article_labels` row for this article, an `ArticleLabel` with `via_terms: true, origin: Origin::User, status: Status::Confirmed`. Existing rows get `via_terms: false`.
  - `untag_article(article, label)`: if the article reaches `label` through a mapped term, upsert a row with `origin='user', status='rejected'` instead of deleting; otherwise delete as today.
  - `merge_labels`: inside the transaction, before deleting `from`: `UPDATE feed_terms SET label_id = ?2 WHERE label_id = ?1`.
  - `delete_label`: nothing to add; the FK `ON DELETE SET NULL` does it (test proves it).

- [ ] **Step 4: Run** `cargo test --workspace` — PASS (existing label filter tests in `app.rs` must still pass).

- [ ] **Step 5: Commit** `git commit -am "Resolve category membership through mapped terms"`

---

### Task 4: Organising API: groups, order, terms, favourites, tree and counts

**Files:**
- Create: `crates/core/src/categories.rs` (`impl Store` block plus types; `pub mod categories;` in `lib.rs`)
- Test: `crates/core/tests/categories.rs`

**Interfaces:**
- Produces (all on `Store`, all `-> Result<_>`):

```rust
pub type GroupId = i64;
pub type TermId = i64;
pub struct Group { pub id: GroupId, pub name: String }
pub struct CategoryNode { pub label: Label, pub group_id: Option<GroupId>, pub term_count: u64 }
pub struct GroupNode { pub group: Group, pub categories: Vec<CategoryNode> }
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Favourite { Group(GroupId), Category(LabelId) }
pub struct CategoryTree { pub groups: Vec<GroupNode>, pub ungrouped: Vec<CategoryNode>, pub favourites: Vec<Favourite> }
pub struct TermRow { pub id: TermId, pub name: String, pub feed_title: String }
#[derive(Default)]
pub struct CategoryCounts { pub groups: HashMap<GroupId, u64>, pub categories: HashMap<LabelId, u64> }

fn category_tree(&self) -> Result<CategoryTree>;            // ordered by position, then name NOCASE
fn create_group(&self, name: &str) -> Result<GroupId>;      // appended last; Err(Invalid) on empty or duplicate name
fn rename_group(&self, id: GroupId, name: &str) -> Result<()>;
fn delete_group(&self, id: GroupId) -> Result<()>;          // categories → ungrouped (FK), favourite removed (FK)
fn move_group(&mut self, id: GroupId, index: usize) -> Result<()>;
fn move_category(&mut self, label: LabelId, group: Option<GroupId>, index: usize) -> Result<()>;
fn category_terms(&self, label: LabelId) -> Result<Vec<TermRow>>;
fn unsorted_terms(&self) -> Result<Vec<TermRow>>;
fn ignored_terms(&self) -> Result<Vec<TermRow>>;
fn map_term(&self, term: TermId, label: Option<LabelId>) -> Result<()>;   // also clears ignored
fn set_term_ignored(&self, term: TermId, ignored: bool) -> Result<()>;   // ignoring clears label_id
fn add_favourite(&self, f: Favourite) -> Result<()>;        // appended last; no-op if present
fn remove_favourite(&self, f: Favourite) -> Result<()>;
fn move_favourite(&mut self, f: Favourite, index: usize) -> Result<()>;
fn category_counts(&self, filter: ListFilter) -> Result<CategoryCounts>;
```

`move_*` renumber positions `0..n` of the affected sibling list in one transaction; `index` is clamped to the list length. `category_counts` counts distinct articles per category and per group using the Task 3 rule with `status = 'confirmed'`, restricted by `filter.view`, `filter.feed_id`, `filter.folder_id` (never by search text). Suggested shape:

```sql
WITH m(article_id, label_id) AS (
  SELECT article_id, label_id FROM article_labels WHERE status = 'confirmed'
  UNION
  SELECT t.article_id, ft.label_id FROM article_terms t JOIN feed_terms ft ON ft.id = t.term_id
  WHERE ft.label_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM article_labels r
        WHERE r.article_id = t.article_id AND r.label_id = ft.label_id AND r.status = 'rejected'))
-- per category: count(*) grouped by label_id; per group: count(DISTINCT article_id) joined to labels.group_id
```

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn tree_orders_groups_and_categories_by_position() {}
#[test] fn group_names_are_unique_ignoring_case_and_spaces() { /* create "Tech", then " tech " → Err */ }
#[test] fn deleting_group_ungroups_categories_and_drops_favourite() {}
#[test] fn move_category_between_groups_renumbers_both() {}
#[test] fn mapping_and_ignoring_terms() { /* map clears ignored; ignore clears mapping; unsorted list excludes both */ }
#[test] fn favourites_keep_their_order() {}
#[test] fn counts_are_distinct_and_follow_view_and_feed() {
    // article in 2 categories of group G → groups[G] == 1; Unread view after marking read → 0;
    // feed filter for another feed → 0
}
```

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** `cargo test -p juttusihti-core` — PASS.

- [ ] **Step 5: Commit** `git commit -am "Add groups, term mapping, favourites and category counts"`

---

### Task 5: The category draft: store, edit, apply

**Files:**
- Create: `crates/core/src/category_draft.rs` (`pub mod category_draft;`)
- Test: `crates/core/tests/categories.rs`

**Interfaces:**
- Produces:

```rust
pub const DRAFT_KIND: &str = "category_plan";
pub const DRAFT_PROMPT_VERSION: &str = "categories-1";
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum GroupRef { Existing(GroupId), New(u32) }
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum CategoryRef { Existing(LabelId), New(u32) }
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
pub struct CategoryDraft {
    pub new_groups: Vec<NewGroupDraft>,          // { temp: u32, name: String, by_user: bool, ticked: bool }
    pub new_categories: Vec<NewCategoryDraft>,   // { temp: u32, name: String, group: GroupRef, ticked: bool }
    pub placements: Vec<Placement>,              // { label: LabelId, rename: Option<String>, group: GroupRef, ticked: bool }  (ungrouped existing categories)
    pub mappings: Vec<Mapping>,                  // { term: TermId, category: CategoryRef, ticked: bool }
    pub vague: Vec<TermId>,
}
pub struct ApplyReport { pub applied: usize, pub skipped: usize }

// Store:
fn category_plan_input(&self, max_terms: usize) -> Result<PlanInput>;
fn save_category_draft(&self, draft: &CategoryDraft, provider: Provider, model: &str) -> Result<i64>; // via record_suggestion, kind DRAFT_KIND
fn pending_category_draft(&self) -> Result<Option<(i64, CategoryDraft)>>;  // newest 'suggested' row of DRAFT_KIND
fn update_category_draft(&self, id: i64, draft: &CategoryDraft) -> Result<()>;
fn discard_category_draft(&self, id: i64) -> Result<()>;   // status 'rejected', decided_at now
fn apply_category_draft(&mut self, id: i64, draft: &CategoryDraft) -> Result<ApplyReport>;
```

`PlanInput { groups: Vec<Group>, categories: Vec<CategoryNode> /* all, with group_id */, terms: Vec<TermRow> /* unsorted, oldest first, at most max_terms */, more_terms: usize }`. Only ungrouped categories and unsorted terms are eligible to change; groups and grouped categories are targets only.

Effective rule (also used by the UI): a new group counts if `ticked`; a new category / placement counts if `ticked` and its group counts (existing groups always count); a mapping counts if `ticked` and its category counts (existing categories always count). A new group with no counting category or placement is dropped.

`apply_category_draft`, one transaction: create counting new groups (if the normalized name already exists, reuse that group), appended last in draft order; create new categories via the existing `create_label` normalized-reuse rule and place them; apply placements only if the label still exists and `group_id IS NULL` (apply `rename` with `rename_label` rules); apply mappings only if the term still exists, `label_id IS NULL` and `ignored = 0`; every reference to a missing existing group/category, or an item no longer eligible, is skipped and counted. Then mark the suggestion `confirmed`, `decided_at = now()`.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn plan_input_contains_only_unsorted_terms_and_flags_more() { /* 5 unsorted, max 3 → 3 terms, more_terms 2; ignored/mapped excluded */ }
#[test] fn draft_round_trips_and_survives_reopen() { /* save, update, pending returns edited draft */ }
#[test] fn apply_creates_groups_categories_and_mappings() {}
#[test] fn unticked_group_drops_its_contents() {}
#[test] fn apply_skips_stale_items() {
    // after saving: user maps one draft term by hand, deletes an existing target group,
    // and groups one placement's category → report.skipped == 3, others applied
}
#[test] fn duplicate_new_group_name_reuses_existing_group() {}
#[test] fn discard_leaves_everything_unchanged() {}
```

- [ ] **Step 2: Run** — FAIL. **Step 3: Implement.** **Step 4: Run** `cargo test -p juttusihti-core` — PASS.

- [ ] **Step 5: Commit** `git commit -am "Add the stored, editable category draft"`

---

### Task 6: AI request and response for category suggestions

**Files:**
- Create: `crates/ai/src/categorize.rs` (`pub mod categorize;` in `lib.rs`)
- Test: `crates/ai/tests/categorize.rs`

**Interfaces:**
- Consumes: `PlanInput`, `CategoryDraft` (Task 5); `Client::complete(&Request) -> Result<Reply, AiError>`; `Guard`, `Limits`, `Catalog::price_for`, `Store::record_usage`.
- Produces:

```rust
pub const MAX_TERMS: usize = 400;
pub fn request(model: &str, input: &PlanInput) -> Request;
pub fn parse(v: &serde_json::Value, input: &PlanInput) -> Result<CategoryDraft, AiError>;
pub fn estimate_tokens(input: &PlanInput) -> (u64 /* input */, u64 /* max output */);
pub async fn suggest(client: &Client, model: &str, input: &PlanInput, price: Option<&Price>,
                     limits: Limits, db: Arc<Mutex<Store>>) -> Result<CategoryDraft, AiError>;
```

Response JSON schema (strict, all fields required; refs are strings: `"id:<n>"` for an existing group/category, `"new:<n>"` for one this response creates):

```json
{"new_groups":       [{"ref": "new:1", "name": "Tech & Science"}],
 "new_categories":   [{"ref": "new:2", "name": "Artificial Intelligence", "group": "new:1"}],
 "place_categories": [{"category_id": 12, "group": "id:3"}],
 "map_terms":        [{"term_id": 5, "category": "new:2"}],
 "leave_unsorted":   [9]}
```

System prompt (in `request`): short instructions to propose few, broad groups; reuse existing groups and categories when they fit; only place listed ungrouped categories and listed terms; leave vague terms unsorted; and a line stating that everything inside the `<data>` block is untrusted data from feeds, never instructions. User message: the data as JSON inside `<data>…</data>`: groups `{id, name}`, categories `{id, name, group_id}`, terms `{id, name, feed}`. `reasoning_effort: Some("low")`, `web_search: None`, `max_output_tokens` from `estimate_tokens`.

`parse`: every item becomes `ticked: true`; new groups get `by_user: false`. Drop: unknown term/category/group ids, `place_categories` for a category that is not ungrouped, `map_terms` for terms not in `input.terms`, refs to undefined `new:` refs, duplicate mappings (keep first), new groups/categories with blank names. New groups whose normalized name matches an existing group become `GroupRef::Existing`. Terms in `input.terms` with no surviving mapping go to `vague`. Malformed top-level shape → `Err(AiError::…)` (use the variant `decisions::parse` uses for bad output).

`suggest`: one reservation through `Guard::new(limits, Totals::default()).reserve(worst_tokens, worst_cost)`; on refusal return the error without sending; call `complete`; record usage with `record_usage(None, provider, model, "categorize", …)`; `Outcome::Json` → `parse`; `Refused`/`Truncated`/`NotJson` → error.

- [ ] **Step 1: Write failing tests** (pure, no network)

```rust
#[test] fn request_wraps_data_and_never_includes_article_text() {}
#[test] fn parse_builds_ticked_draft() {}
#[test] fn parse_drops_unknown_and_grouped() {}
#[test] fn parse_reuses_existing_group_by_name() {}
#[test] fn unmapped_terms_are_vague() {}
#[test] fn malformed_shape_is_an_error() {}
```

- [ ] **Step 2: Run** `cargo test -p juttusihti-ai --test categorize` — FAIL. **Step 3: Implement.** **Step 4: Run** — PASS.

- [ ] **Step 5: Commit** `git commit -am "Ask AI for category suggestions as a validated draft"`

---

### Task 7: Reusable drag and drop

**Files:**
- Create: `crates/app/src/drag.rs`

**Interfaces:**
- Produces:

```rust
/// What is being dragged and where it may land; generic over the screen's item and target types.
pub struct Drag<I, T> { pub item: I, pub label: String, pub start: Point, pub cursor: Point, pub over: Option<T>, pub active: bool }
pub enum DragMsg<I, T> { Press(I, String, Point), Move(Point), Enter(T), Leave(T), Release, Cancel }
pub struct DragState<I, T>(Option<Drag<I, T>>);
impl<I: Clone, T: Clone + PartialEq> DragState<I, T> {
    pub fn update(&mut self, msg: DragMsg<I, T>) -> Option<(I, T)>;   // Some on a drop over a target
    pub fn dragging(&self) -> Option<&Drag<I, T>>;
}
pub const DRAG_THRESHOLD: f32 = 4.0;
pub fn handle<'a, M>(content: Element<'a, M>, on_press: impl Fn(Point) -> M) -> Element<'a, M>;  // ⋮⋮ grip via mouse_area
pub fn target<'a, M>(content: Element<'a, M>, highlighted: bool, on_enter: M, on_exit: M) -> Element<'a, M>;
pub fn ghost<'a, M>(label: &'a str) -> Element<'a, M>;   // floating chip drawn in an overlay `stack!` at the cursor
```

Build with `iced::widget::mouse_area` (`on_press`, `on_enter`, `on_exit`, `on_move`) and a window-level mouse subscription for `Move`/`Release`; Esc sends `Cancel`. A press that moves less than `DRAG_THRESHOLD` before release is a click, not a drop. Target highlight: amber dashed outline (spec mockup); insertion targets render a 2 px amber line.

- [ ] **Step 1: Write failing unit tests** in `drag.rs`

```rust
#[test] fn small_movement_is_a_click_not_a_drop() {}
#[test] fn drop_over_target_returns_item_and_target() {}
#[test] fn leaving_target_clears_it() {}
#[test] fn cancel_drops_nothing() {}
```

- [ ] **Step 2–4:** run `cargo test -p juttusihti drag` — FAIL, implement, PASS.

- [ ] **Step 5: Commit** `git commit -am "Add reusable drag and drop for lists"`

---

### Task 8: Organise screen (manual)

**Files:**
- Create: `crates/app/src/organise.rs` (replaces `labels.rs`; delete `labels.rs` and move its rename/merge/delete/bulk-decide behaviour here)
- Modify: `crates/app/src/app.rs` (`Screen::Labels` → `Screen::Organise`, message routing), `crates/app/src/rail.rs` (tool "Categories", `Icon::Labels`)

**Interfaces:**
- Consumes: Task 4 API, Task 7 `DragState<OrgItem, OrgTarget>`.
- Produces: `pub enum OrgItem { Group(GroupId), Category(LabelId), Term(TermId) }`, `pub enum OrgTarget { GroupSlot(Option<GroupId>, usize), Category(LabelId), Unsorted, Ignored }`; `pub struct State`, `pub enum Msg`, `State::new(db)`, `update`, `view`, matching existing screens.

Layout per mockup `ai-proposal-v3` without the ✨ heading and Approve: groups as headings, categories with ⋮⋮ handle and "N terms ›" (expands to terms with feed name in grey), **+ New group** (inline name field), Ungrouped section, Unsorted terms section, collapsed Ignored section. Click a name to rename inline (Enter saves, Esc cancels). Group delete asks for confirmation ("Delete group "X"? Its categories become Ungrouped."). Each row has a "Move to…" pick list (keyboard fallback). Category merge/delete and bulk decide on pending AI label suggestions keep today's behaviour. A "✨ Suggest categories" button is present but wired in Task 9. Empty state: "No categories yet. Publisher categories appear here after the next refresh."

- [ ] **Step 1: Write failing tests** (in `organise.rs`, with an in-memory `Db` like other screens)

```rust
#[test] fn dropping_category_on_group_moves_it() {}
#[test] fn dropping_term_on_category_maps_it() {}
#[test] fn ignore_and_unignore_term() {}
#[test] fn delete_group_needs_confirmation() {}
#[test] fn move_to_menu_matches_drag() {}
```

- [ ] **Step 2–4:** run `cargo test -p juttusihti organise` — FAIL, implement, PASS; then launch the app (`cargo run -p juttusihti`) and check the screen in light and dark theme.

- [ ] **Step 5: Commit** `git commit -am "Replace the Labels screen with the Categories organise screen"`

---

### Task 9: AI suggestions in the Organise screen

**Files:**
- Modify: `crates/app/src/organise.rs`
- Create (if `organise.rs` passes ~800 lines): `crates/app/src/organise_draft.rs` for draft editing and view

**Interfaces:**
- Consumes: Task 5 store API and effective rule, Task 6 `categorize::{request, estimate_tokens, suggest, MAX_TERMS}`, `ai_setup::defaults`, `ai_setup::readiness`, `research::endpoint`, `Catalog::price_for` (as in `fact_check.rs::plan`).

Flow: **✨ Suggest categories** → consent panel (like `fact_check.rs`): what is sent ("Sends N publisher terms with their feed names, and your category and group names, to {provider}. No article text."), estimated tokens and cost, the per-run limit, **Send** / Cancel; disabled with a reason when nothing is unsorted or ungrouped, or credentials are not ready. After Send: progress line, then the draft is saved (`save_category_draft`) and the screen switches to draft mode. If `more_terms > 0` show "N more terms will be offered next time."

Draft mode (mockup `ai-proposal-v3`): heading "✨ Suggested categories" and "Nothing is saved until you approve."; existing groups shown with tag **yours** and read-only name, their existing categories read-only; AI groups tagged **AI**; tick boxes on new groups, new categories, placements; unticking a group unticks its contents; click to rename new items and placements; drag new categories/placements between any groups (including user groups added with **+ New group**, `by_user: true`), drag terms between categories; one line "N terms were too vague to sort and stay unsorted."; **Cancel** (`discard_category_draft`) and **Approve** (`apply_category_draft`, then message "Applied N changes" plus ", skipped N that changed meanwhile" when skipped > 0). Every edit calls `update_category_draft`. On screen open, `pending_category_draft` restores draft mode. Normal organising controls are hidden while a draft is open. Errors (provider, refused, malformed) show in the panel; nothing is saved.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn consent_sends_nothing_until_send() {}
#[test] fn pending_draft_reopens_in_draft_mode() {}
#[test] fn unticking_group_unticks_contents() {}
#[test] fn dragging_category_into_user_group_updates_draft() {}
#[test] fn approve_reports_skipped() {}
```

- [ ] **Step 2–4:** FAIL, implement, PASS; manual check with a real provider key against the demo database copy (see memory: drive the app via its window, never screen clicks).

- [ ] **Step 5: Commit** `git commit -am "Suggest categories with AI as an editable draft"`

---

### Task 10: Reader sidebar

**Files:**
- Create: `crates/app/src/category_sidebar.rs`
- Modify: `crates/app/src/app.rs` (layout between rail and list, `list_query`, remove the "Any label" pick list and `LabelChoice`, keyboard)

**Interfaces:**
- Consumes: Task 4 `category_tree`, `category_counts`, favourites and `move_*`; Task 7 drag; `Store::setting`/`set_setting`.
- Produces: `pub enum Selection { All, Group(GroupId), Category(LabelId) }`; app field `category: Selection` replacing `label_filter`; `list_query()` sets `group_ids` / `label_ids` from it; setting key `"sidebar.categories.open"` (`"1"`/`"0"`, default open).

Content per mockups `sidebar` / `sidebar-v2`: header "Categories" with **«**; "Find category…" field; **All articles**; **★ Favourites** (hidden when empty); groups (fold state kept in memory) with categories; **Ungrouped** heading (not selectable) with its categories; **Organise…** link to the Organise screen. Counts from `category_counts(ListFilter{view, feed_id, folder_id})` per the current All/Unread and feed picker, reloaded with the sidebar data and after any organise change. Search: case-insensitive `normalize_text` contains-match on group and category names → flat list, group name in grey after each match; Esc or ✕ clears. Favourite toggle: star shown on hover and a right-click context menu entry "Add to favourites" / "Remove from favourites". Drag reorders favourites, groups and categories (within and between groups). Hidden: list header shows **»** and the current selection name; filter stays applied. Keyboard: **c** toggles the sidebar (add to Help's shortcut list). A selection that no longer exists after reload falls back to `All` (replaces `label_filter_drops_a_deleted_label`). Clear (search) no longer clears the category selection.

- [ ] **Step 1: Write failing tests** (update the three existing label-filter tests in `app.rs` to the sidebar)

```rust
#[test] fn selecting_group_filters_by_group() { /* list_query().group_ids == [g] */ }
#[test] fn selecting_category_filters_by_category() {}
#[test] fn deleted_selection_falls_back_to_all() {}
#[test] fn sidebar_open_state_is_remembered() {}
#[test] fn find_shows_flat_matches() {}
#[test] fn c_toggles_sidebar() {}
```

- [ ] **Step 2–4:** FAIL, implement, PASS; launch the app, check open/hidden, light and dark.

- [ ] **Step 5: Commit** `git commit -am "Add the categories sidebar to the reader"`

---

### Task 11: Wording, Help and changelog

**Files:**
- Modify: user-visible strings in `crates/app/src/*.rs` (`grep -rn '"[^"]*[Ll]abel' crates/app/src`: article tag row "Labels" → "Categories", "+ label" → "+ category", "Remove label" → "Remove category", quick AI task name, review screen, research scope, reports headings "Labels" → "Categories", "Labels by source" → "Categories by source"), `crates/app/src/help.rs`, `README.md` feature list, `CHANGELOG.md` `[Unreleased]`
- Test: existing copy tests (`panel_copy`-style) updated

Help additions: the sidebar (hide with « or **c**, favourites, find, drag to reorder); the Categories screen (groups, publisher categories, Unsorted, Ignore); "✨ Suggest categories" (what is sent, consent, nothing saved until Approve); "Publisher categories are collected from now on; articles fetched earlier get them only if they are still in the feed at the next refresh." CHANGELOG `### Added` / `### Changed` entries for the same.

- [ ] **Step 1:** Change strings; `grep -rni "label" crates/app/src` shows no remaining user-visible "label" text except identifiers.
- [ ] **Step 2:** `cargo test --workspace` — PASS; `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [ ] **Step 3: Commit** `git commit -am "Call labels categories and document categories"`

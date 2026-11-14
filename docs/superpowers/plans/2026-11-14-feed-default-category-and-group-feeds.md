# Feed Default Category and Feeds in Groups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A feed can have a default category and can sit in one category group; lists, counts, the sidebar, Organise and the feed edit form follow.

**Architecture:** Two nullable columns on `feeds` (migration 0009). Category and group membership stay computed live in SQL: `category_membership` (search.rs) and the `category_counts` CTE (categories.rs) both gain the feed-default step, and the group filter/count gain "or the article's feed is in the group". The tree carries each group's feeds and each category's default-for feeds, so the sidebar and Organise read them from the existing snapshot.

**Tech Stack:** Rust 2024, rusqlite, Iced 0.14 (custom `drag.rs`).

**Spec:** `docs/superpowers/specs/2026-11-14-feed-default-category-and-group-feeds-design.md`

## Global Constraints

- Membership rule, for an article in feed `f`: manual category when set (only one); otherwise every category its mapped RSS terms point to; if no mapped term category, `f.default_category_id` when set; if neither, its AI category.
- An article is in group G when it is in a category of G **or** its feed's `category_group_id` is G. An article in a group only through its feed, without a category, is still Uncategorized.
- Lists, counts, search and `articles_to_categorize` must all follow the same rule.
- Migration 0009 only adds nullable columns and an index; no data is rewritten. `SCHEMA_VERSION` becomes 9.
- Deleting a category clears defaults pointing at it; deleting a group takes its feeds out of it; merging A into B moves feed defaults from A to B.
- One group per feed; feeds under a group are listed by title (`COLLATE NOCASE`), after the group's categories.
- No live AI calls, no GUI driving. Any app run uses a copied DB via `JUTTUSIHTI_DATA_DIR`.
- `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test --workspace` passes.

## Review Focus

1. A feed with a default whose articles carry only **unsorted** (unmapped) terms: the default applies, they are not Uncategorized and not offered to ✨ (test in Task 1).
2. An article in a group both through a category and through its feed: the group count counts it once (test in Task 2).
3. Deleting the default category or the feed's group: the feed simply loses it; tree and counts load (tests in Tasks 1 and 2).
4. Selecting a sidebar feed row while the feeds-menu scope is a different feed: the list is empty, never the union of both feeds (test in Task 4).
5. List and count disagreeing for the same category under the new rule (parity test in Task 1).

---

### Task 1: Migration, feed default category in the membership rule

**Files:**
- Create: `crates/core/migrations/0009_feed_categories.sql`
- Modify: `crates/core/src/db.rs` (add the migration to `MIGRATIONS`; the test asserting `(8, 8)` becomes `(9, 9)` where it checks the current version)
- Modify: `crates/core/src/model.rs` (`Feed`), `crates/core/src/store.rs` (`FEED_COLS`, `feed_row`)
- Modify: `crates/core/src/search.rs:462-474` (`category_membership`)
- Modify: `crates/core/src/categories.rs` (`category_counts` CTE, `merge_categories`, `article_category`, new setter)
- Modify: `crates/app/src/article_category.rs` (`describe`)
- Modify: `crates/app/src/app.rs:1052` (the `Feed { .. }` literal gets the new fields)
- Test: `crates/core/tests/categories.rs` (append; reuse `entry`, `feed`, `map_term`, `set_ai`, `article_id`, `in_category`, `by_category`)

**Interfaces:**
- Produces:
  - `Feed { …, pub default_category_id: Option<i64>, pub category_group_id: Option<i64> }`
  - `Store::set_feed_default_category(&self, feed: FeedId, category: Option<CategoryId>) -> Result<()>` — `Err(NotFound)` when the feed does not exist.
  - `ArticleCategory { …, pub from_feed: Option<FeedDefault> }` with `pub struct FeedDefault { pub category: Category, pub feed_title: String }` — set only when the default is in effect (no manual, no mapped term category).
  - `article_category::describe(&ArticleCategory) -> (String, Option<String>)` (the note becomes owned so it can name the feed).

- [ ] **Step 1: Write the failing tests** (in `crates/core/tests/categories.rs`)

```rust
#[test]
fn feed_default_covers_articles_without_mapped_terms() {
    // feed f: articles "1" (no terms), "2" (unsorted term "Misc"), "3" (term "AI" mapped to tech)
    // set default sport on f
    assert_eq!(in_category(&s, sport), ["1", "2"]);
    assert_eq!(in_category(&s, tech), ["3"]);
    assert_eq!(by_category(&s, sport), 2);
    let counts = s.category_counts(ListFilter::default()).unwrap();
    assert_eq!(counts.categories.get(&sport), Some(&2));
    assert_eq!(counts.uncategorized, 0);
}

#[test]
fn feed_default_beats_ai_and_clearing_it_brings_ai_back() {
    // article "1" (no terms) has AI category tech; default sport set
    assert_eq!(in_category(&s, sport), ["1"]);
    assert!(in_category(&s, tech).is_empty());
    s.set_feed_default_category(f, None).unwrap();
    assert_eq!(in_category(&s, tech), ["1"]);
}

#[test]
fn mapped_terms_drop_the_ai_category() {
    // article "1" with term "AI" mapped to tech and AI category local: only tech
    assert_eq!(in_category(&s, tech), ["1"]);
    assert!(in_category(&s, local).is_empty());
}

#[test]
fn manual_category_beats_feed_default() {
    // default sport, manual local on "1"
    assert_eq!(in_category(&s, local), ["1"]);
    assert!(in_category(&s, sport).is_empty());
}

#[test]
fn feed_default_articles_are_not_offered_to_ai() {
    // two feeds without terms; f has default sport, g none
    let (offered, eligible) = s.articles_to_categorize(ListFilter::default(), 100).unwrap();
    assert_eq!(eligible, 1); // only g's article
    assert!(offered.iter().all(|a| s.article(a.id).unwrap().feed_id == g));
}

#[test]
fn deleting_or_merging_the_default_category() {
    s.merge_categories(sport, tech).unwrap();
    assert_eq!(s.feed(f).unwrap().default_category_id, Some(tech));
    s.delete_category(tech).unwrap();
    assert_eq!(s.feed(f).unwrap().default_category_id, None);
}

#[test]
fn article_category_reports_the_feed_default() {
    // "1" no terms, default sport -> from_feed Some(sport); "3" mapped term -> from_feed None
}

#[test]
fn list_and_counts_agree_on_every_category() {
    // a mix: manual, mapped, unsorted, AI, default, two feeds; for each category c:
    for c in s.categories().unwrap() {
        let counted = s.category_counts(ListFilter::default()).unwrap().categories.get(&c.id).copied().unwrap_or(0);
        assert_eq!(by_category(&s, c.id), counted, "{}", c.name);
    }
}
```

Plus in `crates/app/src/article_category.rs` tests: `describe` of `ArticleCategory { from_feed: Some(FeedDefault { category: cat(4, "Sport"), feed_title: "Yle Urheilu".into() }), .. }` is `("Sport".into(), Some("From feed Yle Urheilu".into()))`; the existing assertions keep their texts, as `String`s.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p juttusihti-core --test categories feed_default`
Expected: compile failure (`set_feed_default_category`, `default_category_id` missing).

- [ ] **Step 3: Implement**

Migration `0009_feed_categories.sql`, verbatim:

```sql
-- A feed's default category (for feeds that send no categories) and the
-- category group it is filed under. Both optional; nothing is rewritten.
ALTER TABLE feeds ADD COLUMN default_category_id INTEGER REFERENCES categories(id) ON DELETE SET NULL;
ALTER TABLE feeds ADD COLUMN category_group_id INTEGER REFERENCES category_groups(id) ON DELETE SET NULL;
CREATE INDEX feeds_category_group ON feeds(category_group_id);
```

`category_membership(target)`: keep the manual branch; the automatic branch (no manual) becomes: mapped term category `{target}`; OR (no mapped term category at all AND the feed default `{target}`); OR (no mapped term category AND feed default IS NULL AND AI `{target}`). Update the doc comment (number of times `target` appears) and every caller's `repeat_n` count to match. Rewrite the `m` CTE in `category_counts` to the same three automatic sources. `merge_categories` adds `UPDATE feeds SET default_category_id = ?2 WHERE default_category_id = ?1`. `describe` in `article_category.rs`: `from_feed` gives the note `From feed <feed title>`; since the new rule drops AI when terms or a default exist, `article_category` returns `ai: None` in those cases so `describe` stays simple.

- [ ] **Step 4: Run the tests**

Run: `cargo test --workspace`
Expected: all pass (fix any existing test that relied on terms ∪ AI; the spec changes that on purpose).

- [ ] **Step 5: Commit**

```bash
git add -A crates/core crates/app/src/article_category.rs crates/app/src/app.rs
git commit -m "Feed default category: covers articles without mapped terms, before AI"
```

---

### Task 2: Feeds in groups — store, tree and counts

**Files:**
- Modify: `crates/core/src/categories.rs` (`GroupNode`, `CategoryNode`, `CategoryCounts`, `category_tree`, `category_counts`, new setters)
- Modify: `crates/core/src/search.rs` (`group_ids` filter; new `SearchQuery::category_feed_id`)
- Modify: every `GroupNode { .. }` / `CategoryNode { .. }` literal in tests (`crates/app/src/organise.rs`, `organise_draft.rs`, `category_sidebar.rs`, core tests) — add the new fields empty
- Test: `crates/core/tests/categories.rs`

**Interfaces:**
- Consumes: Task 1's `Feed` fields.
- Produces:
  - `pub struct GroupFeed { pub id: FeedId, pub title: String }`
  - `GroupNode { …, pub feeds: Vec<GroupFeed> }` — by title, NOCASE.
  - `CategoryNode { …, pub default_for: Vec<String> }` — titles of feeds using it as default, by title.
  - `CategoryCounts { …, pub feeds: HashMap<FeedId, u64> }` — articles per feed that is in a group, same scope/view as the rest.
  - `CategoryTree { …, pub ungrouped_feeds: Vec<GroupFeed> }` — feeds in no group, by title (for Organise's "Add feed…").
  - `Store::set_feed_group(&self, feed: FeedId, group: Option<GroupId>) -> Result<()>` — `Err(NotFound)` for a missing feed.
  - `Store::find_or_create_group(&self, name: &str) -> Result<GroupId>` — an existing group with the same normalized name is reused, else created last.
  - `SearchQuery { …, #[serde(default)] pub category_feed_id: Option<i64> }` — adds `AND a.feed_id = ?` (intersects with `feed_ids`).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_group_contains_all_articles_of_its_feeds() {
    // group sports with category football; feed f in sports; f's article "1" mapped to politics,
    // "2" no category; feed g article "3" mapped to football
    s.set_feed_group(f, Some(sports)).unwrap();
    let in_group = matching(&s, &SearchQuery { group_ids: vec![sports], ..Default::default() });
    assert_eq!(in_group, ["1", "2", "3"]);
    let c = s.category_counts(ListFilter::default()).unwrap();
    assert_eq!(c.groups.get(&sports), Some(&3));
    assert_eq!(c.feeds.get(&f), Some(&2));
    assert_eq!(c.uncategorized, 1); // "2": in the group only through its feed
}

#[test]
fn a_group_counts_an_article_once_via_feed_and_category() {
    // f in sports, f's "1" mapped to football (in sports)
    assert_eq!(s.category_counts(ListFilter::default()).unwrap().groups.get(&sports), Some(&1));
}

#[test]
fn tree_lists_group_feeds_and_default_users_by_title() {
    // feeds "b.example" and "A.example" in sports; "c.example" default for football, no group
    let t = s.category_tree().unwrap();
    assert_eq!(t.groups[0].feeds.iter().map(|f| f.title.as_str()).collect::<Vec<_>>(), ["A.example", "b.example"]);
    assert_eq!(t.groups[0].categories[0].default_for, ["c.example"]);
    assert_eq!(t.ungrouped_feeds.iter().map(|f| f.title.as_str()).collect::<Vec<_>>(), ["c.example"]);
}

#[test]
fn deleting_a_group_releases_its_feeds() {
    s.delete_group(sports).unwrap();
    assert_eq!(s.feed(f).unwrap().category_group_id, None);
    assert!(s.category_tree().unwrap().groups.is_empty());
}

#[test]
fn find_or_create_group_reuses_by_normalized_name() {
    let a = s.find_or_create_group("Urheilu").unwrap();
    assert_eq!(s.find_or_create_group(" urheilu ").unwrap(), a);
}

#[test]
fn category_feed_id_intersects_with_the_feed_scope() {
    let q = SearchQuery { feed_ids: vec![g], category_feed_id: Some(f), ..Default::default() };
    assert!(matching(&s, &q).is_empty());
}
```

Counts with `ListFilter { feed_id: Some(g), .. }` must leave `feeds[f]` absent or 0.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p juttusihti-core --test categories group`
Expected: compile failure.

- [ ] **Step 3: Implement**

Group filter: `(category_membership("IN (SELECT id FROM categories WHERE group_id = ?)") OR f.category_group_id = ?)`, binding the id the extra time. Group counts: `COUNT(DISTINCT article_id)` over the union of (scoped memberships joined to their category's group) and (scoped articles whose feed has a group). `uncategorized` is unchanged (category membership only).

- [ ] **Step 4: Run the tests**

Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add -A crates
git commit -m "Feeds in category groups: tree, counts and group filter include them"
```

---

### Task 3: Feed edit form — default category and group

**Files:**
- Modify: `crates/app/src/app.rs` (`FeedEdit` ~141, its `Message`s, open/save ~1442-1490)
- Modify: `crates/app/src/feeds_menu.rs:178` (`edit_form`, and `view` signature to receive the category tree)
- Test: `crates/app/src/app.rs` tests module

**Interfaces:**
- Consumes: `set_feed_default_category`, `set_feed_group`, `find_or_create_group`, `Feed` fields, `category_sidebar::State`'s `CategoryTree`.
- Produces: `FeedEdit { id, title, folder, new_folder, default_category: Option<CategoryId>, group: Option<GroupId>, new_group: String }`; messages `FeedDefaultCategory(CategoryChoice)`, `FeedGroup(GroupChoice)`, `NewGroup(String)` (pick-list wrapper types in the style of `FolderChoice`).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn saving_a_feed_sets_its_default_category_and_a_new_group() {
    // open edit on feed f, pick default `sport`, type new group "Urheilu", SaveFeed, run tasks
    let feed = with_store(|s| s.feed(f)).unwrap();
    assert_eq!(feed.default_category_id, Some(sport));
    let g = feed.category_group_id.unwrap();
    assert_eq!(with_store(|s| s.category_tree()).unwrap().groups.iter().find(|x| x.group.id == g).unwrap().group.name, "Urheilu");
}
```

Also: opening the form preselects the feed's current default and group; "No default category" / "No group" clear them. Use the existing app test helpers for opening the edit form and running tasks.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p juttusihti saving_a_feed_sets`
Expected: compile failure.

- [ ] **Step 3: Implement**

Two rows under the folder row: pick list "No default category" + categories by name; pick list "No group" + groups in tree order, with text input placeholder `or new group`. Save, in the existing single `db.call`: folder as today, `update_feed`, `set_feed_default_category`, then group = `find_or_create_group(new_group)` when the input is non-blank, else the pick. `FeedSaved(Ok)` also reloads the category sidebar (counts change).

- [ ] **Step 4: Run the tests**

Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/app/src
git commit -m "Feed edit form: default category and category group"
```

---

### Task 4: Category sidebar — feed rows under groups

**Files:**
- Modify: `crates/app/src/category_sidebar.rs` (`Selection`, `count`, `matches`, `view_group`/`view_members`, icon)
- Modify: `crates/app/src/app.rs` (`list_query` ~674)
- Modify: `crates/app/src/icons.rs` if a feed icon is needed
- Test: `crates/app/src/category_sidebar.rs` and `crates/app/src/app.rs` tests

**Interfaces:**
- Consumes: `GroupNode::feeds`, `CategoryCounts::feeds`, `SearchQuery::category_feed_id`.
- Produces: `Selection::Feed(FeedId)`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn selecting_a_group_feed_lists_its_articles() {
    select(&mut app, Selection::Feed(f));
    assert_eq!(app.list_query().category_feed_id, Some(f));
    // list shows only f's articles
}

#[test]
fn a_group_feed_under_another_feed_scope_lists_nothing() {
    // feeds-menu scope Scope::Feed(g), select Selection::Feed(f) -> empty list
}

#[test]
fn find_matches_group_feeds_by_title() {
    // find "ars" -> a Match with selection Selection::Feed(f) and group Some("Teknologia")
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p juttusihti group_feed`
Expected: compile failure.

- [ ] **Step 3: Implement**

Feed rows after the group's category rows, same indent, a feed icon before the title, count from `counts.feeds`. Feed rows are clickable only: not draggable, not drop targets, no favourite menu. `Selection::Feed` survives a reload only while the feed is still in some group (as `contains` does for categories). `list_query` sets `category_feed_id` for `Selection::Feed`.

- [ ] **Step 4: Run the tests**

Run: `cargo test --workspace`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/app/src
git commit -m "Category sidebar: feeds under their groups"
```

---

### Task 5: Organise — feeds in groups, defaults shown; help and changelog

**Files:**
- Modify: `crates/app/src/organise.rs` (`OrgItem`, `Op`, `Msg`, `drop_op`, group view, category row)
- Modify: `crates/app/src/help.rs`, `CHANGELOG.md`
- Test: `crates/app/src/organise.rs` tests

**Interfaces:**
- Consumes: `GroupNode::feeds`, `CategoryTree::ungrouped_feeds`, `CategoryNode::default_for`, `set_feed_group`.
- Produces: `OrgItem::Feed(FeedId)`; `Op::SetFeedGroup(FeedId, Option<GroupId>)`; `Msg::AddFeed(GroupId, FeedId)`, `Msg::RemoveFeed(FeedId)` (names may follow the file's conventions).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn adding_dragging_and_removing_a_group_feed() {
    // Msg::AddFeed(sports, f) -> f in sports
    // drag OrgItem::Feed(f) onto OrgTarget::GroupSlot(Some(tech), 0) -> f in tech
    // drag onto GroupSlot(None, _) -> no group
    // Msg::RemoveFeed(f) after re-adding -> no group
}

#[test]
fn dropping_a_feed_on_its_own_group_does_nothing() {
    assert_eq!(state.drop_op(OrgItem::Feed(f), OrgTarget::GroupSlot(Some(sports), 0)), None);
}

#[test]
fn a_feed_cannot_be_dropped_on_categories_or_term_targets() {
    // drop_op for OrgTarget::Category(_), Unsorted, Ignored -> None
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p juttusihti organise`
Expected: compile failure.

- [ ] **Step 3: Implement**

In each group, after its categories: feed rows (icon, title, "Remove from group"), draggable as `OrgItem::Feed`; while a feed is dragged, only group slots (and Ungrouped) are drop zones; then an "Add feed…" pick list of `ungrouped_feeds` (hidden when empty). A category row with `default_for` shows a muted line `Default for: <titles joined by ", ">`. The group's empty text changes to "Empty. Drag categories or feeds here." Help: a short paragraph on feed defaults (set in the feed's edit form; used when the feed's terms give no category; before AI) and feeds in groups (all of the feed's articles show in the group). CHANGELOG: one line under the unreleased section.

- [ ] **Step 4: Run the tests and lints**

Run: `cargo fmt --all; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace`
Expected: clean, all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/app/src CHANGELOG.md
git commit -m "Organise: feeds in groups, feed defaults shown; help and changelog"
```

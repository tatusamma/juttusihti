# Feed default category and feeds in category groups

## Intent

Some feeds send no RSS categories but stay on one theme (a sports feed, a
tech blog). The user wants to:

1. give such a feed a **default category**, so its articles land in that
   category without the AI, and
2. put a **feed directly into a category group**, next to the group's
   categories, so the group shows everything from that feed.

Both serve the same goal as categories overall: quick filtering, with less
AI spend and fewer uncategorized articles.

## Goals

- A feed can have one default category, set in the feed's edit form.
- A feed can sit in one category group, set in the feed's edit form (with
  "New group…" to make a group and file the feed in one step) or in
  Organise.
- Feeds in a group show in the category sidebar under that group, after its
  categories, with a feed icon; clicking one lists that feed's articles.
- A group's list and count include **all** articles of its feeds, whatever
  their categories (the user's decision).

## Non-goals

- A feed in several groups, or hand-ordering feeds inside a group (feeds
  are listed by name).
- Feeds in Favourites.
- Changing labels, folders or the RSS-term mapping.

## Anti-goals

- The AI must never be sent articles that a feed default already covers.
- Setting or clearing a default must not lose an article's manual or AI
  category: the AI category is kept apart and comes back when the default
  is cleared.
- Old databases must keep working: no data is rewritten.

## Approaches considered

1. **Two nullable columns on `feeds`** (chosen): `default_category_id` and
   `category_group_id`. Membership is worked out live in SQL, like today,
   so a default covers old articles too and a change takes effect at once.
   One group per feed, like one folder per feed.
2. **A group↔feed table with positions**: a feed in several groups,
   draggable order. More code (move, reorder, remove) for nothing asked.
3. **"Feed in group" as a hidden per-feed category used as default**:
   rejected; a default covers only articles without another category, which
   breaks "all of the feed's articles count in the group".

## Decided design

### Data (migration 0009)

```sql
ALTER TABLE feeds ADD COLUMN default_category_id INTEGER REFERENCES categories(id) ON DELETE SET NULL;
ALTER TABLE feeds ADD COLUMN category_group_id  INTEGER REFERENCES category_groups(id) ON DELETE SET NULL;
CREATE INDEX feeds_category_group ON feeds(category_group_id);
```

Both start NULL. Deleting a category clears defaults pointing at it;
deleting a group takes its feeds out of it. Merging category A into B moves
defaults from A to B (as term mappings already move).

### Category membership (one rule, used by lists, counts, search and AI eligibility)

For an article in feed `f`:

1. Its manual category, when set: that is its only category (unchanged).
2. Otherwise: every category its mapped RSS terms point to (unchanged), and
3. if it has no mapped term category: `f.default_category_id`, when set;
4. if it has neither mapped terms nor a feed default: its AI category.

Today's rule unions terms and AI; with this change the AI category counts
only when neither terms nor a default give one. "Uncategorized" means no
category by this rule, so articles covered by a default are no longer
Uncategorized and no longer offered to ✨.

`category_membership` (`crates/core/src/search.rs`) and the CTE in
`category_counts` (`crates/core/src/categories.rs`) must both implement
exactly this rule; a test should pin that they agree.

### Group membership

An article is in group G when it is in a category of G **or** its feed's
`category_group_id` is G. This applies to the group filter (`group_ids` in
`build_where`) and to group counts (distinct articles). An article that is
in a group only through its feed and has no category is still
Uncategorized, and ✨ may still give it a category.

### Article category panel

`ArticleCategory` gains `from_feed: Option<Category>` (the feed default,
present only when it is in effect by the rule above), and the article's
category panel shows it with a line like "From feed <title>".

### Feed edit form (`crates/app/src/feeds_menu.rs`, `FeedEdit` in `app.rs`)

Two new rows under the folder row:

- **Default category**: pick list "No default category" + all categories by
  name.
- **Category group**: pick list "No group" + groups in order, with a
  "or new group" text input like "or new folder". Save creates the group
  (existing normalized name is reused) and files the feed.

Saving goes through one store call that sets title, folder, default and
group together.

### Category sidebar (`crates/app/src/category_sidebar.rs`)

- Under each group, after its categories: one row per feed in the group,
  by title, with a feed icon and its article count in the current scope.
- `Selection::Feed(FeedId)` lists that feed's articles (combined with the
  feeds-menu scope and view like other selections).
- Feed rows are not draggable and not favouritable in the sidebar. Search
  in the sidebar matches feed rows by title too.

### Organise (`crates/app/src/organise.rs`)

- Each group shows its feeds after its categories, with a feed icon and a
  "Remove from group" action.
- Each group has an "Add feed…" pick list of feeds not in any group.
- A feed row can be dragged onto another group (moves it) or onto
  Ungrouped (removes it from its group).
- Each category row shows the feeds using it as default ("Default for:
  Ars Technica"), read-only; it is set in the feed form.

### AI

No prompt changes. `articles_to_categorize` follows from the rule (articles
with a feed default are not Uncategorized). The Suggest step is unchanged.

### Help and changelog

Help text for categories explains feed defaults and feeds in groups; one
CHANGELOG line.

## Constraints

- Rust 2024, Iced 0.14, rusqlite; migrations via `PRAGMA user_version`
  (this makes SCHEMA_VERSION 9).
- No live AI calls and no GUI driving by implementers; tests only.
- Any run of the app uses a copied database via `JUTTUSIHTI_DATA_DIR`.

## Left to the builder

- Exact wording of labels and help text, the feed icon choice, layout of the
  new form rows.
- Store API names and how the form save is split into calls.
- SQL shape, as long as lists and counts follow the one rule above.

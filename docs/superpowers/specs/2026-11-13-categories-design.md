# Categories: groups, publisher terms and a reader sidebar

Date: 2026-11-13. Status: draft for approval.

## Intent

Feeds come with many publisher category words ("Politiikka", "Tech", "AI", "Kotimaa", ...), often
the same idea in several spellings and languages. The user wants a **short, calm list** to
browse news by: a few **groups**, each holding a few **categories**, with the publisher's words
quietly mapped underneath. The key is that the user is never overwhelmed by categories.

They want to arrange it by hand, and optionally ask AI to suggest an arrangement that they can
edit freely (rename, untick, add their own groups, drag things around) before approving.

This replaces an earlier brief written without knowledge of this codebase. Its main parts that
are **not** carried over: a separate "category group" system parallel to labels, an in-memory AI
preview outside the app's consent/review rules, "Regroup All with AI", feed-level category
fallback, and any DuckDB work.

## The model (decided)

Three levels:

```
Group                       e.g. Tech & Science        (new; organisation only)
  └ Category                e.g. Artificial Intelligence   (today's "labels", renamed in the UI)
       └ Publisher term     e.g. "Tekoäly" (Yle), "AI" (Coastline Courier)   (new; collected from feeds)
```

- **Labels are renamed to categories** everywhere the user sees them: the filter, the article's
  tag row, the Labels screen, Help, reports and CSV headers. Database tables and Rust names stay
  `labels` / `article_labels` (builder's call whether to rename Rust types; the database is not
  renamed, to avoid a migration with no user value).
- **Groups** are new. A category belongs to **at most one** group; one with no group shows
  under **Ungrouped**. Groups have a user-chosen order; categories have an order within their
  group.
- **Publisher terms** are collected automatically during feed refresh, scoped per feed (the same
  word from two feeds is two terms, because they can mean different things). Each term is in one
  of three states: **unsorted** (new), **mapped** to one category, or **ignored**.
- **An article is in a category** if it has a confirmed label for it (as today, by the user or a
  confirmed AI suggestion), **or** one of its publisher terms is mapped to that category. The
  mapping is resolved at query time, not copied onto articles, so remapping a term changes the
  lists immediately without a refresh.
- **A per-article "no" wins** *(my call)*: a `rejected` `article_labels` row for that article
  and category overrides a term mapping. The article's tag row shows term-derived categories
  too; removing one there writes a `rejected` row (instead of deleting) so it stays removed.
  `include_suggested` does not affect term-derived links (mappings are always user-approved).
- **An article is in a group** if it is in any category of that group. An article appears once
  per list regardless of how many terms or categories match.
- Existing labels become categories with no group (they appear under Ungrouped).

## Collecting publisher terms

- Use `feed-rs` entry categories, which cover RSS 2.0 `<category>`, Atom `<category>` and JSON
  Feed `tags` alike. Store the displayed text (Atom `label` if present, else `term`) and the
  RSS `domain` / Atom `scheme` when present.
- Item/entry categories only. **No feed-level fallback** (channel categories are not attached to
  articles).
- Identity: `(feed, normalized text, normalized domain-or-scheme)` with the domain stored as `''`
  when absent, so SQLite's NULL-in-UNIQUE behaviour cannot create duplicates. Normalize with the
  existing `normalize_text` (keeps ä/ö/å distinct).
- Re-fetching is idempotent: no duplicate terms, no duplicate article–term links.
- Skip empty/whitespace values; cap absurdly long ones (builder picks the limit).
- New terms start **unsorted**. Refresh never changes a mapping and **never calls AI**.
- Article–term links are deleted with the article (follow existing `ON DELETE CASCADE` and
  retention). A term with no remaining articles may be kept (builder's call; keeping it is
  simpler and preserves the user's mapping).
- **Old articles:** terms were never stored, so only articles fetched after this ships carry
  them. A one-off re-parse is not possible because the raw feed is not stored; accept this and
  say so in Help.

## Organise screen (the Categories screen)

Replaces today's Labels screen and is opened from the rail and from **Organise…** at the
bottom of the sidebar. Agreed look (mockup `ai-proposal-v3`): calm, minimal chrome.

- Groups as headings, categories under them, each with a ⋮⋮ drag handle and a count.
  Publisher terms stay hidden behind "N terms ›" until a category is opened.
- **+ New group**: adds an empty group; type its name.
- Rename a group or category by clicking its name and typing.
- **Drag** a category to another group or to Ungrouped; drag to reorder groups and categories;
  drag a publisher term to another category. A **Move to…** action does the same from the
  keyboard (fallback and accessibility).
- Delete a group (with confirmation): its categories become Ungrouped; nothing else is deleted.
- Delete and merge categories keep today's label behaviour, plus *(my call)*: deleting a
  category returns its mapped terms to **unsorted**; merging moves the source's term mappings to
  the target, and the target keeps its own group and position.
- An **Unsorted** section lists unsorted publisher terms (with their feed name in grey) so they
  can be dragged onto a category or marked **Ignore**. Ignored terms are reachable (e.g. a
  collapsed "Ignored" section) and can be un-ignored.
- The screen works fully offline and without any AI provider configured.

## AI suggestions

One button on the Organise screen: **✨ Suggest categories**.

- **Scope: only new and ungrouped things.** Unsorted publisher terms and ungrouped categories.
  It never proposes changes to groups, categories or mappings the user has already arranged, and
  never touches ignored terms. (On first use everything is ungrouped, so it sees everything.)
- It may suggest new groups, new categories, placing ungrouped categories into existing or new
  groups, and mapping unsorted terms to existing or new categories. It may leave vague terms
  unsorted.
- What is sent: term texts with their feed titles, category names, group names. No article text.
  Feed and term text is treated as untrusted data in the prompt.
- Follows the app's AI rules: shows a local estimate and asks for consent before sending, counts
  against spending limits, uses the configured provider. Never runs on refresh, startup or edits.
- The result opens **in the Organise screen as an editable draft** (mockup `ai-proposal-v3`):
  - Heading "✨ Suggested categories — nothing is saved until you approve".
  - Everything ticked by default; unticking a group unticks everything in it.
  - Each group tagged **AI** or **yours**. The user can rename anything, untick, add their own
    groups with **+ New group**, and drag categories and terms between AI and own groups.
  - A suggested group left empty is dropped.
  - *(my call)* The draft holds **only the AI's proposal plus groups the user adds in it**.
    The user's existing groups and categories appear as drop targets (tagged **yours**) but
    cannot themselves be moved or renamed while the draft is open; Approve writes only the diff.
    While a draft is pending, the Organise screen shows it and normal organising waits until
    the user approves or cancels.
  - One quiet line: "N terms were too vague to sort and stay unsorted."
  - **Cancel** discards; **Approve** applies the ticked, edited draft in one SQLite transaction.
- The draft is stored (in `ai_suggestions`, kind e.g. `category_plan`) so closing the app does
  not lose it. On approve, every referenced term/category/group is re-checked; anything that no
  longer exists or is no longer unsorted/ungrouped is skipped, and the user is told how many.
- Malformed AI output or a provider error shows an error and changes nothing.

## Reader sidebar

Agreed look: mockups `sidebar` and `sidebar-v2`.

- A panel between the icon rail and the article list. **«** hides it, **»** (in the list header)
  shows it, plus a keyboard shortcut. Open/closed is remembered (settings table).
- Contents top to bottom: **All articles**, **★ Favourites**, then the full tree
  (groups fold open/closed; then Ungrouped), then **Organise…**.
- Clicking a group lists articles in any of its categories; clicking a category narrows to it.
  The list header names the current selection; the filter stays applied when the sidebar is
  hidden.
- Counts are distinct articles and follow the existing All / Unread switch and the feed
  picker, but **not** the article search text *(my call: keeps counts cheap and stable while
  typing)*.
- **Favourites:** a group or category can be favourited (right-click menu, or a star on hover).
  Favourites show above the tree and still appear in their usual place. Drag to reorder them.
- **Drag to reorder** groups and categories in the sidebar too; it is the same order as on the
  Organise screen.
- **Find category…** box filters category and group names as you type, showing matches as a flat
  list with their group in grey; Esc or ✕ restores the tree. It does not search articles.
- Combines with the existing feed picker, All/Unread, and article search.
- Replaces the "Any label" pick list next to the search box.

## Constraints

- SQLite is the only store for all of this; new migration `0008_categories.sql` in
  `crates/core/migrations`. IDs are `INTEGER`, times are UTC unix seconds, like the rest.
- Group names unique ignoring case and surrounding whitespace.
- Article filtering extends the existing query in `crates/core/src/search.rs` with `EXISTS`
  semi-joins, keeping its sorting, paging, search and read filters.
- Indexes so that group/category filtering and counts stay fast with tens of thousands of
  articles.
- Iced 0.14 has no built-in drag and drop; it must be built (one reusable piece used by the
  sidebar and the Organise screen).
- Follow the app's existing style (warm paper colours, amber accent, light and dark themes,
  Lucide icons) and keyboard conventions.

## Analytics

DuckDB reports already copy `labels` and `article_labels`; with the rename they become category
reports and need only text changes. Categories reached only through publisher-term mappings are
**not** added to reports in this version (follow-up).

## Goals

- A short, browsable sidebar of groups and categories, hideable, with favourites, search and
  drag to reorder.
- Publisher terms collected automatically and mapped to categories by hand or with AI.
- AI suggestions that the user can edit freely before approving.

## Non-goals

- AI reorganising what the user already arranged ("Regroup All").
- Nested groups deeper than group → category.
- A category in more than one group.
- AI classifying articles during refresh, embeddings, recommendations.
- Feed-level category fallback.
- DuckDB changes beyond renamed wording.

## Anti-goals (failure even if it works)

- The sidebar fills up with dozens of raw publisher words.
- Anything gets rearranged without the user pressing something.
- An AI call happens without the estimate and consent step.
- The Organise or AI screen looks busy (the first AI mockup was rejected for this).
- Reading, refresh or manual organising stops working when AI is unavailable.

## Left to the builder

Table and column layout; how drag and drop is implemented in Iced; the AI prompt and response
schema (validated against real IDs, like the existing `decisions` parsing); exact keyboard
shortcut; empty states; tests (term extraction for RSS/Atom/JSON, idempotent re-fetch, filtering
returns each article once, approve applies only valid ticked items, nothing runs AI on refresh).

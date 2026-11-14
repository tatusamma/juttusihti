# Categories separate from labels

Date: 2026-11-14. Status: approved in conversation; revises
`2026-11-13-categories-design.md` (read that first — everything there still holds unless this
document changes it).

## Why

While testing the branch the user decided that labels and categories are two different things:

- **Labels** (the user's own and AI labelling) serve **search and analytics**.
- **Categories** serve **quick filtering** of the news: a short list in the sidebar.

Treating labels as categories (the 2026-11-13 design) mixed the two: 192 AI-suggested labels
flooded the category tree, counts were 0 because labels were unconfirmed suggestions, and AI
labelling (several labels per article) is the wrong tool for picking a category.

## Decisions (the user's)

1. **Labels go back to what they were before this branch.** Called "Labels" in the UI again:
   the label filter next to the search box, the Labels screen (rename, merge, delete, bulk
   decide), AI labelling in the ✨ quick panel and custom analysis, Review, reports and CSV
   (headers and file names back to `label`/`labels`), Help, README. All existing labels (the user
   has 192) stay labels. Labels never appear in the categories sidebar or Organise screen.
2. **Categories are their own entity**, in groups, starting **empty**.
3. An article's categories come from, in priority order:
   1. **Manual**: the user sets or changes an article's category in the article view.
   2. **RSS**: publisher terms mapped to categories (as in the 2026-11-13 design).
   3. **AI**: only for articles whose feed gave **no** publisher terms — the ✨ on Uncategorized,
      100 articles per press, **one main category per article**, saved directly (no Review),
      may create a new category when none fits.
4. The user may modify an article's category.

## Builder's calls made in conversation (confirmed by the user's "yes")

- **Manual wins and is single**: setting a category by hand gives the article exactly that one
  category, overriding RSS and AI ones. The user can clear the manual choice to return to
  automatic (RSS/AI). There is no manual "none": clearing returns to automatic.
- **RSS can give several**: if an article's terms map to different categories it is in each.
- **AI gives one**: the AI result is a single category per article.
- **Reports** do not include categories in this version; they use labels as before this branch.
- The branch is unreleased, so its schema migrations are **restructured** (not stacked):
  categories get their own tables; `labels` is left exactly as on master. The user's test copy
  is re-copied from their real database; the real database is never opened by the test build.

## Model

```
Group                    (category_groups, as now)
  └ Category             (NEW table categories: name, normalized UNIQUE, group_id, position)
       └ Publisher term  (feed_terms.category_id instead of label_id)
```

An article is **in category C** when:
- it has a **manual** category and that category is C; otherwise
- one of its publisher terms is mapped to C (no per-article rejection is needed any more —
  manual override replaces "reject"); or
- it has an **AI** category and that category is C.

Storage *(builder's call)*: `article_categories(article_id PK, user_category_id NULL,
ai_category_id NULL, updated_at)` — the manual choice and the AI's main category are kept in
separate columns, so clearing a manual choice returns to the AI one (and RSS) without losing it.
Both FKs `ON DELETE SET NULL`; a row with both NULL may be deleted. If terms get mapped later
for an article that also has an AI category, both apply.

**AI eligibility** *(builder's call)*: an article is eligible for the ✨ on Uncategorized when it
is Uncategorized **and** none of its publisher terms is mapped or unsorted — i.e. it has no
terms, or only **ignored** ones (a feed that sends only junk like "Tilaajille" counts as giving
no category). Articles with unsorted terms wait for the user (or ✨ Suggest categories) to map
those terms. Articles AI already checked are not re-sent (as now); setting or clearing a manual
category does not reset that.

**AI-created categories** *(builder's call)*: matched to existing categories by normalized name
before creating; new ones go to Ungrouped, last.

**Merge** *(builder's call)*: merging A into B moves term mappings and both article columns
from A to B; favourites of A vanish.

**Migrations** *(builder's call)*: the branch's 0008/0009 are rewritten freely (numbering is the
builder's); `labels` and every other master table are untouched; nothing from a branch-era
database is carried over (the test copy is re-copied from the real database). Group membership: in any category
of the group. Counts are distinct articles, follow All/Unread and the feed picker, not search
text (as now). **Uncategorized** = articles in no category by this rule.

## What changes versus the current branch

- Everything that currently points categories at `labels` (`labels.group_id`, `labels.position`,
  `feed_terms.label_id`, `favourites.label_id`, the draft's `CategoryRef::Existing(LabelId)`,
  `Selection::Category(LabelId)`, `SearchQuery.label_ids` used for categories, the membership
  rule over `article_labels`, AI-checked articles) points at `categories` instead.
- `SearchQuery` gets `category_ids` (and keeps `group_ids`, `uncategorized`, `without_terms`);
  `label_ids` returns to its master meaning (confirmed `article_labels` only).
- The Organise screen manages categories (create, rename, delete, merge, group, terms). Label
  management returns to the Labels screen.
- The ✨ on Uncategorized: prompt and schema change from "1–3 categories" to **exactly one
  category or none** per article; results stored as `article_categories` `ai` rows.
- ✨ Suggest categories (the editable draft): unchanged in behaviour, but over categories
  (ungrouped categories + unsorted terms). The AI is told nothing about labels.
- Article view: a **Category** control (current category or "None", a pick list of categories
  plus "Automatic" to clear a manual choice); shows where an automatic category came from
  (from the feed / from AI) in muted text.
- Deleting a category: its terms return to Unsorted, `article_categories` rows for it are
  removed (manual and AI), favourites of it vanish. Merging moves terms and article rows.

## Unchanged from the current branch

Term collection, groups, favourites, drag and drop, sidebar (search, favourites, Organise… at
the top, «/»/c), Uncategorized and its ✨ panel flow (consent, 100 per press, one request,
split on context errors, checked articles not re-sent), Organise layout, the AI draft flow and
its safeguards, counts performance work.

## Anti-goals

- Labels leaking into the categories sidebar or Organise screen, or categories into the label
  filter.
- Losing any of the user's labels or label decisions.
- AI giving an article more than one category.
- AI running on articles that have publisher terms.

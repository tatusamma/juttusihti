# Categories: your groups as the frame, any language

Date: 2026-11-14. Status: approved in conversation (short design). Builds on
`2026-11-14-categories-separate-design.md`; everything there still holds.

## Why

The user makes their own top-level groups (Politics, Sport, …) and wants the AI to fill them
with categories. Feeds come in several languages; a Finnish article or term about football
belongs in the user's one "Sport" category, not in a new "Urheilu" next to it.

## Already true (unchanged)

- An article is in every category its feed's terms map to (RSS can give several).
- AI (✨ on Uncategorized) only gets articles whose feed gave no terms (or only ignored ones);
  an article with an RSS category is never sent to the AI. AI gives one main category.

## Decisions (the user's)

- **Option B:** the AI works within the user's existing groups first; in ✨ Suggest categories
  (the editable draft) it may still propose a new group, which the user can untick or rename
  before approving. The instant ✨ on Uncategorized never creates groups.
- Terms and articles in any language go to the matching existing category.

## Changes

1. **✨ Suggest categories** (`crates/ai/src/categorize.rs`): the prompt says existing groups
   are the user's frame — place categories into them first; propose a new group only for
   categories that fit none. Map terms in any language to the existing category with the same
   meaning (e.g. "Urheilu", "Sport", "Sports" → one category); never create a category that
   duplicates an existing one in another language. Name new groups and categories in the
   language of the existing group and category names. Request shape, schema and parsing
   unchanged (an AI group whose name matches an existing one is already reused).
2. **✨ on Uncategorized** (`crates/ai/src/categorize_articles.rs` and its caller):
   - The data sent lists groups (`id`, `name`) and existing categories with their `group_id`,
     instead of bare names.
   - The answer per article becomes `{"id", "category", "group"}`; `group` is `"id:<n>"` of an
     existing group or `""`. It only matters when `category` is new.
   - Prompt: match articles in any language to the existing category with the same meaning;
     prefer existing categories strongly; a new name is in the language of the existing names
     and must name one existing group when one fits.
   - Applying: a new category goes into the named group (last position) if that group exists;
     an unknown or empty group means Ungrouped (as today). Existing categories are matched by
     normalized name as today and never moved.
   - When split into halves, the second half sees the first half's new categories with their
     groups.

## Builder's calls

- The language rule is wording in the prompt only; no language detection or setting in code.
  When there are no existing names, new names follow the articles' language (as today).
- No database change: every existing group counts as the user's frame.
- The prompt version strings move on (`categories-3` for the draft) so old drafts are not
  mixed with new ones, if the code keys on that.

## Anti-goals

- The ✨ on Uncategorized creating groups, or moving an existing category.
- Near-duplicate categories in different languages created by the AI.
- AI touching articles that have RSS terms.

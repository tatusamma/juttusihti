# Calm UI: reading-first redesign

Date: 2026-10-31. Applies to the Iced 0.14 desktop app in `crates/app`. The code currently lives on
`tero-k/feat-ai-analysis-budget` (worktree `halibut`).

## Why

The app works, but the whole thing feels busy and tiring to look at. It grew one feature at a time
(reader, research, review, reports, map, timeline, AI) and every feature got equal visual weight.
Screenshots taken 2026-10-31 showed:

- an 11-item tool menu sitting above the feeds in the sidebar, at the same weight as "All articles";
- a strip of tiny grey text buttons above the article list (✨ AI, Show original headlines, Hide
  dismissed, Mark all read, ‹ ›, Edit, Unsubscribe);
- 13 different text sizes in the code, mostly 11–13 px, so nothing stands out and everything is small;
- the stock `TokyoNight` / `Light` themes, whose bright cyan is used for the selected item, Refresh,
  Open in browser, tabs and report bars alike, so it is the loudest thing on screen and means nothing;
- two button styles only: heavy grey slabs and near-invisible text links;
- the ✨ AI panel pushing the list down and overflowing its box;
- research and setup screens opening with paragraphs of fine print.

**Reading the day's news is the main job. The research tools are used less often.** The redesign
puts reading first and moves everything else out of its way, without removing any feature.

## Goals

- User friendly, simple, easy on the eyes. A calm screen where the article list and the article are
  the only things that draw attention.
- One consistent visual language across every screen: one colour family, one accent, few text sizes,
  three button kinds.
- Every existing feature stays reachable.

## Non-goals

- No new features, no changes to data, AI, fetching or analytics behaviour.
- No change to keyboard shortcuts (j/k, arrows, s, m, o/Enter, /, n, p, Ctrl+F, Ctrl+R).
- Not a redesign of how the research tools work internally (Timeline, Map and Relationship map
  canvases keep their logic; they only get the new colours, controls and header style).

## Anti-goals (would make it a failure even if it works)

- A feature becomes hard to find or disappears.
- Colour creeps back: several accents, or the accent used on things that are not the current
  selection, unread state, saved state or the one main action.
- Text gets smaller to fit more in. Secondary text is made dimmer, not smaller.
- The reading pane is not the most comfortable thing to look at.

## Decided with the user

### Layout: icon rail, two panes ("B")

```
┌─────────────────────────────────────────────────────────────────────┐
│ Juttusihti   [ Search news…                               ]   ⟳  ⚙  │  top bar
├──┬──────────────────────────┬───────────────────────────────────────┤
│☰ │ All feeds ▾ · Unread  ✨ ⋯│ Helsingin Sanomat · 27.10.2026 22:10 │
│★ │──────────────────────────│ Ovensuukyselyt: Netanjahun Likud…     │
│▤ │ ● Headline               │ ☆ Save   Open in browser   Mark unread│
│✓ │   Source · 21:48         │                                       │
│◷ │──────────────────────────│ Body text, comfortable size and width │
│⌖ │   Headline               │                                       │
│⋈ │   Source · 20:55         │ Labels                                │
│  │                          │                                       │
│⚙ │                          │                                       │
└──┴──────────────────────────┴───────────────────────────────────────┘
 rail   article list               article
```

- **Top bar:** app name, one large search box, refresh icon button, settings icon button. The
  Light/Dark toggle moves into Settings.
- **Icon rail (left, narrow):** News (default, selected on start), Saved, then the research tools:
  Reports, Review, Timeline, Map, Relationship map, Custom analysis. AI setup, Labels and Settings
  sit at the bottom of the rail. Each icon shows its name in a tooltip on hover. The current item
  is marked with the accent colour.
- **List scope (designer's call):** the feeds dropdown and the All / Unread toggle belong to News
  only. Saved is one fixed list of all saved articles, with no feed scope. News remembers its feed
  and Unread choice for the session, so coming back from Saved or a research screen restores it.
  "Mark all read", the "AI for: …" label and the AI run scope follow the current News scope (or
  Saved when on Saved), as today.
- **Feeds in a dropdown above the list:** "All feeds ▾" chooses all feeds, a folder or a single
  feed (each with its unread count), with an All / Unread toggle beside it. Adding a feed,
  importing and exporting OPML, editing and unsubscribing a feed move into that dropdown (an
  "Add feed…" entry and a per-feed edit action) or into Settings. The permanent "Site or feed URL"
  box at the bottom of the sidebar goes away. Because Unsubscribe now sits next to rows you click to
  switch feeds, it asks for confirmation once ("Unsubscribe from X?") — the one deliberate behaviour
  change (designer's call).
- **Article list (middle):** headline, then source and time on one quiet line. Unread articles get
  a small accent dot and a slightly heavier headline. The selected article gets a soft highlight
  with a thin accent edge. Above the list only two controls: ✨ AI and a ⋯ menu. "Mark all read",
  "Hide/Show dismissed", "Show original headlines (n rewritten)" and the page arrows move into ⋯
  (page arrows may stay as small icons at the list's foot instead — builder's call).
- **Article (right):** source and date (small, dim), large headline, a quiet row of Save / Open in
  browser / Mark unread, plus a quiet "Connections" button (today's "Map" button, which opens the
  Relationship map focused on the article; same behaviour, renamed so it is not confused with the
  geographic Map — designer's call), then body text in a comfortable size,
  line height and limited line width. Labels go below the text. AI-rewritten headline controls
  (Keep / Use original, "Original: …") stay with the headline but in the quiet style.
- **Research screens** take the whole area right of the rail. Clicking News on the rail returns to
  reading.

### Look: warm paper & amber ("A")

Both themes are custom Iced themes replacing `TokyoNight` and `Light`. Dark stays the default.
Starting values (from the approved mockup; the builder may fine-tune for contrast, but must keep
the character — warm brown-greys, one amber accent):

| Token | Dark | Light | Used for |
|---|---|---|---|
| background | `#1c1b1a` | `#faf7f2` | window |
| pane | `#222120` | `#f1ece4` | rail, inputs, popovers |
| line | `#2f2d2b` | `#e4ddd2` | dividers |
| text | `#ebe6df` | `#2a2622` | headlines, primary text |
| body | `#d6d1c9` | `#3d3832` | article body |
| muted | `#9b958c` | `#7a7268` | secondary text, icons |
| faint | `#716b64` | `#9a9288` | timestamps, placeholders |
| selection | `#2b2926` | `#efe6d8` | selected row / item |
| accent | `#d8a25a` | `#b0702a` | see below |

Plus a warm success green and danger red, toned down to match, for confirm/reject and destructive
actions only.

**Amber is the only accent.** It marks: the selected article and current rail item, unread dots,
Save / saved state, and the single main button on a screen (for example "Send to AI", "Start
analysis", "Add feed"). Nothing else in the UI chrome is coloured. Report bars use toned-down shades
from the same warm family, not cyan.

**Data colours (designer's call):** the Map, Relationship map and Timeline canvases need to tell
categories apart (today: event places/conflicting links in danger red, publisher places/ordinary
links in primary, entity places/event links in success green). They get a small, separate, muted
category palette — for example amber, sage green, clay red and dusty slate, in dark and light
variants — defined next to the theme and used only inside visualisations. Canvases stop reading
`palette.primary/danger/success` for categories. The encodings themselves (what is which category)
do not change.

### Text

Four sizes replace the current thirteen (approximate; builder sets exact values):

- article headline ≈ 24
- screen titles and list headlines ≈ 15–18 (screen title may be a step larger than list headline)
- body text ≈ 15–16, line height ≈ 1.6, max line width about 70 characters
- small ≈ 12–13 for source, time, hints, counts — never below 12

Secondary text is dimmer (muted / faint), not smaller. Long explanations on research and setup
screens shrink to one short line, or move behind a small "?" that shows the full text on hover.
No information is deleted, only demoted.

### Buttons and controls

- **Three button kinds:** one amber main button where a screen needs one; quiet outlined buttons
  for everything else; icon buttons for the rail and top bar. The heavy grey slabs and the
  near-invisible text-link buttons both go. Destructive actions (Unsubscribe, Remove key, Reject
  all) use the quiet style with the danger colour, never the main style.
- **Segmented controls** for one-of-many choices: 7 / 30 / 90 days / 1 year in Reports,
  Week / Month / Quarter / Year in Timeline, Entities / Relationships / Relevance / Summary in
  Review, and similar. One piece highlighted.
- More breathing room: roomier list rows, consistent spacing between sections on every screen.
- Status messages (the bar at the bottom) use the pane colour with muted text; errors use the danger
  colour.

### ✨ AI panel

Becomes a popover that opens from the ✨ button and floats over the list instead of pushing it
down. Contents top to bottom: title ("AI for: All articles") with Setup and close; task checkboxes
in one tidy column; model line; estimate line; the amber send button; the one-line note about what
is sent. Below that, everything the panel shows today keeps its place: the progress line,
Pause / Cancel during a run, and the result line with its "Review" link afterwards.

Closing (designer's call): clicking outside or pressing Esc closes the popover at any time,
including during a run — the run keeps going. While a run is in progress the ✨ button shows a
running indicator (e.g. a small amber dot or "✨ 3/12"); clicking it reopens the popover with
progress and Pause / Cancel. When a run finishes while the popover is closed, the result line and
"Review" link also appear in the status bar. The popover keeps following the current list scope
as it does today.

## Left to the builder

- Icons: which icon set (must be bundled with the app and render identically on Windows and Linux;
  an icon font such as Lucide or a few SVGs are both fine), and the exact glyph for each rail item.
- Rail width, tooltip style, exact spacing scale, corner radii.
- Exact text sizes and colour fine-tuning within the rules above, including meeting readable
  contrast for faint text in both themes.
- Whether OPML import/export lives in the feeds dropdown, in Settings, or both.
- Where page navigation (n/p) lives visually.
- How the refactor is structured in code (a shared `style`/`theme` module with named button,
  container and text styles is the obvious route), and the order screens are migrated.
- Font: keep the default unless a bundled reading font clearly helps; if one is bundled, it must
  cover Finnish (ä, ö, å) and the license must allow bundling.

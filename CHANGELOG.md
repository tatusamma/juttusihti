# Changelog

All notable changes to Juttusihti are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release workflow copies the section for a tag into the GitHub Release notes, so add a
`## [X.Y.Z] - YYYY-MM-DD` section before tagging (see [RELEASING.md](RELEASING.md)).

## [Unreleased]

### Fixed

- **OpenAI API key without credit**: a run now pauses at once with a message saying the account
  has no credit and where to add it, instead of retrying and failing. Resume it under Custom
  analysis after topping up. The ✨ panel now also shows why a run stopped.
- Some messages (the clickbait-check estimate note, the ChatGPT plan estimate warning and the
  clickbait help in AI setup) and the headline rewriting prompt no longer contain long runs of
  spaces.

## [0.2.0] - 2026-11-01

### Changed

- **Clickbait headlines** (OpenAI with an API key): a fast clickbait check on OpenAI's
  Decisions API (`gpt-6-luna`) now rates every headline first, in batches, and only headlines
  rated at least 70% likely clickbait are sent to the rewriting model. Ratings are cached, so a
  new threshold costs nothing, and articles without a description are no longer sent at all.
  If the check fails, headlines go to the rewriting model as before. Turn the check off or
  change its threshold under AI setup.
- AI requests now start as soon as an earlier one returns, instead of in waves of four.

## [0.1.1] - 2026-11-01

### Fixed

- **Windows**: starting Juttusihti no longer opens a terminal window next to the app.

## [0.1.0] - 2026-11-01

The first release: a local-first RSS reader with optional, consent-based AI research tools,
for Windows and Linux.

### Added

- **Reader**: subscribe by site or feed address with feed discovery; RSS 2.0, Atom and JSON
  Feed; folders, feed rename and move; OPML import and export; All, Unread and Saved views;
  scheduled and manual refresh with conditional requests; offline reading; retention
  clean-up that always keeps saved and researched articles.
- **Search**: local SQLite FTS5 full-text search with phrases, `OR`, `NOT`, prefixes,
  `title:`/`author:`/`summary:`/`content:` fields and `NEAR`, with highlighting. A label menu
  next to the search box narrows the list to one confirmed label.
- **Labels**: add labels from the article preview; rename, merge and delete them on the Labels
  screen; accept or reject AI label suggestions one by one or all at once.
- **AI for a list (✨)**: one click runs labels, clickbait headline rewrites, related stories,
  people and places, or a summary on the list you are looking at. A local estimate of
  articles, requests, tokens and cost comes first, and the send button is your consent.
- **Custom analysis**: choose a date range, feeds, folders, labels or a search, the model,
  content depth, tasks and limits; the preview is computed locally and the analysis waits for
  approval before anything is sent.
- **Providers**: OpenAI, Anthropic (Claude) and Gemini with your own API key, kept in the
  system's secret store, or Sign in with ChatGPT to run OpenAI requests on a ChatGPT plan.
  Fast and Quality model defaults and per-run spending limits; runs can be paused, resumed
  and cancelled and never exceed the limits.
- **Clickbait headlines rewritten** from the article text, shown with an AI tag and the
  original underneath; switch back per article or for the whole list.
- **Review**: everything AI produces is a suggestion until you confirm it. Entities,
  relationships, relevance and summaries are reviewed one by one or in bulk, and a later AI
  run never overrides your choice. Hiding irrelevant articles is reversible.
- **Reports** (DuckDB): volume per source, topics, labels, labels by source, co-occurring
  entities, relevance and clickbait trends, story links, and AI usage and cost, over 7 to 365
  days in day, week or month buckets, with CSV export.
- **Timeline, map and relationship map** of stories, events, places, people and
  organisations. OpenStreetMap tiles are opt-in and follow the tile usage policy; without them
  a coordinate grid is shown and nothing is sent.
- **Calm interface**: an icon rail, warm paper colours with amber accents in light and dark
  themes, text scale, Lucide icons, an optional CPU renderer that uses far less memory, and a
  built-in Help screen (**F1**).
- **Keyboard shortcuts**: `j`/`k`, `s`, `m`, `o`, `n`/`p`, `/`, `Ctrl+F`, `Ctrl+R`, `Esc`,
  `F1`.
- **Packages**: a signed per-user Windows installer and portable zip; Linux `.deb`, `.rpm`,
  AppImage and tarball built on GitHub Actions with SHA-256 checksums and build provenance
  attestations.

### Security

- All data stays in one SQLite database on your computer. Nothing is sent to an AI provider or
  a tile server unless you ask.
- Model output is treated as untrusted: schemas are enforced and checked again locally, and
  evidence must appear in the article text.
- The DuckDB instance is locked down (no extensions, files or network) and has no free-form
  SQL entry point. The threat model and dependency audit are in
  [SECURITY.md](https://github.com/tero-k/juttusihti/blob/master/SECURITY.md).

### Known issues

- The AI providers and Sign in with ChatGPT have been tested against local stand-ins of their
  APIs only, not yet against the live services. Please report anything that fails.
- On Linux, the app has been run under WSL (Wayland and X11) but not yet on native GNOME or
  KDE desktops, and the system secret store there is untested.
- Reports use duckdb-rs 1.20000, which bundles a development build of DuckDB 2.0.
- FTS5 has no Finnish stemmer: search inflected words with a prefix, such as
  `työttömyysturva*`.

[Unreleased]: https://github.com/tero-k/juttusihti/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/tero-k/juttusihti/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/tero-k/juttusihti/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/tero-k/juttusihti/releases/tag/v0.1.0

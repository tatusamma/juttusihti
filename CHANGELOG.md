# Changelog

All notable changes to Juttusihti are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release workflow copies the section for a tag into the GitHub Release notes, so add a
`## [X.Y.Z] - YYYY-MM-DD` section before tagging (see [RELEASING.md](RELEASING.md)).

## [Unreleased]

## [0.5.0] - 2026-11-08

### Added

- **Fact check**: when an article seems off, press "Fact check" in the reader (after "Read full
  article") to have your AI check its factual claims. It picks up to 8 claims, searches the web
  with the provider's own search, and looks up Wikipedia and, with an optional key, published
  fact checks. Each claim gets a verdict: Supported, Contradicted, Misleading, Unverified or Not
  checked (limit reached), with a short reason and source links.
- **Verdicts need real evidence**: a verdict must rest on a source actually found during the
  check. The article's own site and copies of the same story do not count, and Wikipedia is
  background only; otherwise the claim stays Unverified.
- **Journalists' guidelines**: the article is also checked against four points of the Finnish
  journalists' guidelines (JSN 15 headline backed by text, 12 source criticism, 21 right to
  reply, 16 hidden advertising).
- **A score you can open**: the result shows as one line above the article, such as "Fact check:
  3 of 5 supported · 1 misleading · 1 unverified"; click it to see each claim. A note under the
  score reminds that the check was made with AI and is a helpful guide, not a guarantee. A red
  "!" in the article list marks articles with contradicted or misleading claims.
- **You stay in control**: nothing is sent until you press `Check with <provider>` in a panel that
  shows the model, what goes where and the cost estimate. Each check has its own limit ($1.00 by
  default, under AI setup → Fact check); claims that no longer fit are marked Not checked. On a
  ChatGPT plan the plan's own limits apply, and all claims are checked at once.
- **AI setup → Fact check**: the limit per check and an optional Google Fact Check Tools API key
  (free from Google Cloud), kept in the system's secret store. If Google rejects the key, the
  result says so.
- Articles in any language can be checked; explanations are in English. How the check works in
  detail is in [docs/fact-check.md](https://github.com/tero-k/juttusihti/blob/master/docs/fact-check.md).

### Changed

- Full-article error messages now speak of the browser engine instead of a "browser helper", and
  a missing engine suggests reinstalling Juttusihti.

### Known issues

- Fact check has been tried against the real service only with OpenAI on a ChatGPT plan. OpenAI
  with an API key, Anthropic and Gemini are covered by tests against local fake servers.
- Fresh news is often Unverified: independent sources may not have reported it yet, and
  published fact checks rarely exist for it.
- A check of 8 claims takes about 20 to 40 seconds, and each claim's call uses roughly 10,000 to
  27,000 input tokens, mostly search results added by the provider.

## [0.4.0] - 2026-11-07

### Added

- **Full articles**: when a feed only gives a teaser, press "Read full article" (or `a`) to read
  the whole story inside Juttusihti. The main picture fades in behind the headline, and the
  article then opens instantly, also offline. Nothing is fetched until you press the button.
- **Private by design**: each fetch uses a hidden Servo browser with a brand-new, empty profile.
  Known trackers and ads are blocked (EasyList and EasyPrivacy), and cookie banners are accepted
  for you, including the free choice of "consent or pay" walls, so you never click one. When the
  text and picture are saved, the whole profile, cookies included, is deleted, so the next visit
  cannot be linked to this one. Block lists and consent rules ship with the app and are never
  downloaded at run time.
- **Paywalls respected**: subscriber-only articles keep only the part the site shows everyone,
  with a note. Hidden and paid parts of a page are never read.
- **Searchable and ready for AI**: fetched text is included in search, and in Custom analysis
  when you choose "Archived article text".
- **Settings → Full articles**: turn picture downloads off for text only, set the cache size
  limit (500 MB by default; the oldest fetched articles fall back to their teaser first, and Saved
  articles are never removed to make room), or clear the cache.
- **Built-in browser engine**: Juttusihti now includes the Servo engine to load article pages.
  It runs in the background only while an article is being fetched, has no window of its own,
  and is installed and updated together with the app (`juttusihti-browser` in the install
  folder; signed on Windows). Building from source needs Rust 1.88 and one extra build command;
  see the [README](https://github.com/tero-k/juttusihti#build-from-source).
- The README and the Help screen (F1) explain full articles step by step, and the README
  screenshots now use made-up demo news.

### Security

- `arrow` updated to 60.1.0 (RUSTSEC-2026-0442). The browser engine's own dependency audit
  is in [SECURITY.md](https://github.com/tero-k/juttusihti/blob/master/SECURITY.md).

### Known issues

- A fetch takes about 5 to 10 seconds. Servo cannot report HTTP errors, so a site that refuses
  the visit shows "Couldn't find the article text on the page."
- Tested against Ilta-Sanomat and Helsingin Sanomat; other sites may lay out their articles in
  ways the extractor misses.
- Full articles on Linux have been checked in CI on Ubuntu 22.04 but not yet on a desktop
  machine. With no GPU, the browser engine needs Mesa's software OpenGL (`libegl-mesa0`,
  `libgl1-mesa-dri`), which the packages depend on.
- The installed app is about 125 MB larger, because it now includes a whole browser engine.

## [0.3.0] - 2026-11-02

### Added

- **Split billing for OpenAI**: "Pay with" has a third choice, ChatGPT plan with API credit for
  the clickbait check. The check goes through the API key and everything else counts against the
  plan, so plan runs can skip rewriting headlines that aren't clickbait. Estimates and run
  results show the API credit separately. If the credit runs out, the run continues on the plan
  without the check. Not yet tried against the real service.

### Fixed

- **"database lock poisoned" errors**: a headline with an `&` followed by text such as `ä` could
  crash the feed reader, and every database action then failed until the app was restarted. That
  headline now reads normally. Feeds are also read before the database is locked, so a feed that
  fails to read affects only itself.
- **Crash details are kept**: the app writes crash messages to `crash.log` in its data folder.

## [0.2.1] - 2026-11-01

### Fixed

- **OpenAI API key without credit**: a run now pauses at once with a message saying the account
  has no credit and where to add it, instead of retrying and failing. Resume it under Custom
  analysis after topping up. The ✨ panel now also shows why a run stopped. Help and the
  README explain the clickbait check, API credit and where to resume a paused run.
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

[Unreleased]: https://github.com/tero-k/juttusihti/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/tero-k/juttusihti/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/tero-k/juttusihti/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/tero-k/juttusihti/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/tero-k/juttusihti/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/tero-k/juttusihti/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/tero-k/juttusihti/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/tero-k/juttusihti/releases/tag/v0.1.0

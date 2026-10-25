# Juttusihti

A local-first RSS reader and news research desktop app written in Rust with Iced.
SQLite is the source of truth and DuckDB runs analytics. Optional AI-assisted research
(OpenAI, Anthropic, Gemini) is planned and only ever starts when the user asks for it.

## Workspace

| Crate | Purpose | Network | AI |
| --- | --- | --- | --- |
| `crates/core` (`juttusihti-core`) | Domain model, SQLite schema + migrations, FTS5 search, feed parsing, OPML, discovery, research data model | no | no |
| `crates/fetch` (`juttusihti-fetch`) | HTTP fetching: conditional requests, timeouts, bounded retries, size limits, discovery | yes | no |
| `crates/analytics` (`juttusihti-analytics`) | DuckDB reports over a consistent SQLite snapshot, on a bounded, cancellable worker | no | no |
| `crates/app` (`juttusihti`) | Iced 0.14 desktop UI | via `fetch` | no |

## Build and run

```sh
cargo test --workspace          # no network or paid API needed
cargo run -p juttusihti         # data in the OS data dir; override with JUTTUSIHTI_DATA_DIR
cargo build -p juttusihti --release
```

Requirements: Rust 1.85+ (2024 edition) and a C/C++ toolchain, because SQLite and DuckDB are
bundled and compiled from source. On Windows this means MSVC (VS 2022 Build Tools); CMake and
NASM are not needed. A first build of bundled DuckDB takes several minutes. Debug info is
turned off for dependencies (`[profile.dev.package."*"]`): with it on, the debug `target/`
grew past 16 GB.

Logging: `JUTTUSIHTI_LOG=debug`.

## Milestone 0 findings (verified 2026-10-25, Windows 11 x64, rustc 1.99.1, MSVC 14.44)

| Component | Version | Result |
| --- | --- | --- |
| iced | 0.14.0 | Builds; app window starts. Defaults to wgpu with a tiny-skia fallback. |
| rusqlite | 0.41.0, bundled SQLite 3.54.1 | FTS5 available. |
| duckdb-rs | 1.20000.0 | Builds and links in the same binary as bundled SQLite (no symbol clashes). **Reports `v2.0.0-dev88614`: a DuckDB development build, not a stable release.** |
| DuckDB `sqlite` extension | — | **Unavailable**: duckdb-rs does not bundle it, and `INSTALL sqlite` returns HTTP 404 from extensions.duckdb.org for this dev build (`cargo test -p juttusihti-analytics --test extension_probe -- --ignored`). |
| reqwest | 0.13 (rustls, aws-lc-rs) | Builds on Windows without CMake or NASM. |
| feed-rs / opml | 3.0.0 / 1.1.6 | RSS 2.0, Atom and JSON Feed fixtures parse. |

Decisions that follow from these findings:

- **The DuckDB snapshot is the integration path, not a fallback.** The analytics worker opens
  SQLite read-only, copies the analytical tables inside a single read transaction into an
  in-memory DuckDB, then runs fixed report queries there. It never holds a SQLite write lock.
- **The DuckDB instance is locked down**: extension autoload and autoinstall are off, external
  file and network access are disabled, memory and thread limits are set, and
  `lock_configuration` is on. Tests check that `INSTALL`, `LOAD`, `ATTACH`, `COPY TO`,
  `read_csv` and attempts to change these settings all fail. There is no free-form SQL entry
  point.
- duckdb-rs 1.20000 is a production risk because it bundles a dev build. Before release,
  either wait for a stable DuckDB 2.0 or pin `duckdb = "~1.10505"` (DuckDB 1.5.x). Nothing in
  this repo depends on 2.0-only features.
- Bucketing by time zone uses a fixed UTC offset. DST-correct zones need the ICU extension,
  which is not bundled.
- **Not verified yet: Linux (GNOME/KDE, Wayland/X11).** All results above are Windows-only.

### Finnish search

The FTS5 tokenizer is `unicode61 remove_diacritics 0`, so `sää` (weather) and `saa` (gets) stay
distinct and case-folding works for ä/ö/å. FTS5 has no Finnish stemmer, so inflected forms
need prefix search: `työttömyysturva*` matches `työttömyysturvaan`, but
`työttömyysturva` does not. Better options to evaluate later: a Snowball/Voikko custom
tokenizer, or AI-assisted query expansion.

## Status by milestone

- **M0 Feasibility**: done on Windows; Linux not yet verified.
- **M1 RSS reader**: done except saved-searches UI. Subscriptions, discovery, folders, feed
  rename and move to folder, All/Unread/Saved views, read/unread state, bookmarks, manual and
  scheduled refresh (interval in Settings), offline reading, OPML import/export, FTS5 search
  with highlighting, ETag/Last-Modified conditional requests, timeouts, bounded retries,
  deduplication, browser hand-off, migrations, retention (Settings; at least 7 days, applied at
  startup), keyboard shortcuts (j/k, s, m, o, `/`, Ctrl+F, Ctrl+R, n/p), and a dark/light theme
  that is remembered between runs.
- **M2 Research data model**: store API done, with tests. Covers labels (normalization,
  similarity checks, merge, bulk decisions), entities with aliases, events, typed relationships,
  locations with coordinate provenance, annotations, AI suggestions with provenance, sessions
  with an enforced lifecycle, result cache and usage ledger. UI: a Labels screen (rename, merge,
  delete, accept/reject all pending suggestions) and label chips in the article preview (add,
  remove, confirm or reject a suggestion). Entities, Events and Relationships screens are not
  built yet.
- **M3 Research interface**: done. The New Analysis screen covers: inclusive local-date range
  (stored as half-open UTC), quick ranges, feeds/folders/labels/query scope, an option to
  exclude undated articles, provider and model, content depth, tasks, relevance topic, limits
  (articles, tokens, USD), batch and cache reuse, and accepting unknown pricing. It previews
  locally (matched, distinct, cached, to process, candidate pairs, requests per stage) and saves
  sessions as `awaiting_approval` with their article snapshot and estimate. There is a session
  list.
- **M5 Token estimator**: the offline parts are done.
  - Pricing catalog: `crates/core/pricing/catalog.json`, integer micro-USD, prices with
    validity dates (Gemini has scheduled increases), long-context tiers, batch multipliers,
    manual overrides. Prices verified 2026-10-25 from the official Anthropic, OpenAI and Gemini
    pricing pages.
  - Request plan, priced per request: one combined extraction pass in batches of 20,
    validation of locally generated candidate pairs only (never all pairs), and a hierarchical
    summary.
  - Optimistic, expected and conservative scenarios, with a reasoning-token allowance for
    thinking models, and a worst-case token bound.
  - Budget guard: reserves each call's worst case so in-flight calls cannot exceed the limit,
    refuses unknown cost unless the user accepted it, and supports pause and daily/monthly
    prior spend from the ledger.
  - Not done: provider token-counting endpoints (they need network access and consent), and
    calibrating the local token approximation against real tokenizers.
- **M4, M6–M8**: not started. No AI provider code exists, so nothing can incur cost; a saved
  session cannot be run yet.

## Invariants enforced in code and tests

- AI-originated records start as `suggested`. A later AI suggestion never overrides a user's
  confirmation or rejection.
- Source article columns are written only by feed ingestion, never by research code. Search and
  AI input use derived `summary_text` and `content_text`; the original HTML is kept as-is.
- Search input is parsed into an AST and rendered with every term quoted, so it cannot inject
  FTS5 operators or SQL. Filters are bound parameters.
- Unsafe input is rejected or bounded: only http(s) URLs are accepted from feeds, OPML,
  discovery and redirects; feeds and OPML have size limits; HTML becomes plain text for display.
- Unknown AI cost is stored as `NULL`, never zero, and reports propagate it as unknown.
- An analysis cannot reach `running` without an estimate and an approval step. Selecting a
  time range never sends anything anywhere: scope, dedup, cache lookup, candidate pairs and
  estimates are all computed locally.

# Juttusihti

A local-first RSS reader and news research desktop app written in Rust with Iced.
SQLite is the source of truth and DuckDB runs analytics. Optional AI-assisted research
(OpenAI, Anthropic, Gemini) is planned and only ever starts when the user asks for it.

## Workspace

| Crate | Purpose | Network | AI |
| --- | --- | --- | --- |
| `crates/core` (`juttusihti-core`) | Domain model, SQLite schema + migrations, FTS5 search, feed parsing, OPML, discovery, research data model | no | no |
| `crates/fetch` (`juttusihti-fetch`) | HTTP fetching: conditional requests, timeouts, bounded retries, size limits, discovery | yes | no |
| `crates/ai` (`juttusihti-ai`) | OpenAI / Anthropic / Gemini clients, prompts and output validation, the approved-session runner | yes, only for approved runs | yes |
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

Logging: `JUTTUSIHTI_LOG=debug`. Renderer: GPU by default; `ICED_BACKEND=tiny-skia` or
Settings → CPU renderer uses far less memory (see BENCHMARKS.md).

Linux build dependencies:
- Debian/Ubuntu: `libdbus-1-dev libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev
  libxi-dev libxrandr-dev libfontconfig1-dev libfreetype-dev`
- Fedora: `dbus-devel libxkbcommon-devel wayland-devel libX11-devel libXcursor-devel libXi-devel
  libXrandr-devel fontconfig-devel freetype-devel`

### Packages

```sh
cargo install cargo-packager --locked
cargo packager --release -f nsis -c Packager.toml            # Windows: per-user NSIS installer
APPIMAGE_EXTRACT_AND_RUN=1 cargo packager --release -f deb,appimage -c Packager.toml   # Linux
```

- **Build Linux packages on Ubuntu 22.04** (glibc 2.35), for example in a container. Built on
  Fedora 44 instead, the binary needs glibc 2.43 and will not run on Ubuntu 24.04 (glibc 2.39).
- **`name` is set in `Packager.toml` on purpose.** Without it, cargo-packager 0.11.8 tries to
  change directory into the config file.
- **CI:** `.github/workflows/ci.yml` runs fmt, clippy (`-D warnings`), tests and `cargo audit`
  on Windows and Ubuntu 22.04. Tags `v*` also build the packages.

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
  (articles, tokens, USD), cache reuse, and accepting unknown pricing. Provider batch APIs are
  not supported yet: runs and estimates both use standard prices. It previews
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
- **M4 AI analytics**: done; tested against fake local servers only.
  - Provider clients:
    - Anthropic Messages API (`anthropic-version: 2026-10-21`, `output_config.format`).
    - OpenAI Responses API (`text.format` with a strict JSON schema; `store: false`).
    - Gemini `generateContent` (`responseMimeType` + `responseJsonSchema`).
    - Provider token counting is implemented for all three.
  - Runner, per approved session:
    - Tasks: label, entity (with location roles) and relevance extraction in batches of 20;
      validation of locally generated story pairs; a hierarchical summary with cited article ids.
    - Results are stored only as AI suggestions with provenance (provider, model, prompt
      version, evidence).
    - Each call reserves budget before it is sent, and actual usage is recorded in the ledger.
    - Pause, cancel and resume are supported. A request cancelled mid-flight is charged at its
      worst case, and finished articles are cached so resuming does not repeat paid work.
  - Model output is untrusted:
    - Article text is escaped inside a data block.
    - The output schema is enforced, then checked again locally: only ids from the request
      are accepted, and string lengths are bounded.
    - Entities must occur in the article, and evidence quotes must appear in the text.
    - Summary points need valid citations.
  - API keys live in the OS secret store (Windows Credential Manager, or the Secret Service on
    GNOME/KDE), set on the AI Providers screen. The screen also explains, step by step, how to
    create a Claude key (Claude Console) and a Gemini key (Google AI Studio), including Gemini's
    free-tier data use.
  - OpenAI runs can instead be charged to a ChatGPT Plus/Pro plan with Sign in with ChatGPT
    (OpenAI's flow for open-source local apps: browser sign-in, PKCE, loopback redirect on
    127.0.0.1, no client secret). Only the rotating refresh token is stored, in the secret
    store; access tokens stay in memory. On a plan, requests stream and cannot cap output
    length, token counting is unavailable, and estimates show "on your ChatGPT plan" (a known
    zero API cost) instead of dollars. Reaching the plan's limit pauses the run. Tested
    against a local fake of OpenAI's endpoints only; **not tried against the real service
    yet**, and structured JSON output on a plan is not documented by OpenAI.
  - Claude and Gemini have no such option: Anthropic's terms forbid third-party apps from
    using Claude subscription sign-in, and Google's sign-in would still bill a Cloud project.
  - Starting a run requires a consent checkbox naming the provider and the content depth.
  - Live API tests are opt-in and ignored by default (`crates/ai/tests/live.rs`). **Not run
    yet.** The Gemini `responseJsonSchema` field in particular still needs live confirmation.
  - Linux builds of the secret store need `libdbus-1-dev` (Debian/Ubuntu) or `dbus-devel`
    (Fedora).
- **Review of AI suggestions**: done. The Review screen covers entities, relationships (which
  can be retyped), relevance and the summary, with cited articles. You can confirm or reject
  items one at a time or in bulk. Relevance classifications live in their own table, so
  hiding is reversible: the reader's "Hide dismissed" filter acts only on classes you have
  confirmed.
- **M6 DuckDB**: done.
  - Reports: volume per source, topic evolution, labels, labels by source, entity
    co-occurrence, relevance/clickbait trends, story-link statistics, AI usage by model and
    cost per day (unknown when any call is unpriced).
  - 7/30/90/365-day windows with day/week/month buckets, and a cancel button.
  - CSV export with formula-injection guarding.
  - Not done: benchmarks at 10k/100k/1M articles.
- **M7 Visual research**: done (relationship map, timeline, geographic map).
  - Relationship map: Canvas graph capped at 150 nodes, with a hop limit, filters,
    zoom/pan, selection and links drawn solid when confirmed and dashed for AI suggestions.
  - Timeline: source lanes, story threads, events with known dates only, a volume histogram,
    zoom, pan and filters.
  - Geographic map: OpenStreetMap raster tiles, following the
    [tile usage policy](https://operations.osmfoundation.org/policies/tiles/).
    - Tiles stay off until the user enables them, with a notice that the viewing area is
      sent to the tile server. Without tiles, a coordinate grid is shown and nothing is sent.
    - Only tiles in the current viewport are requested, after a pan or zoom ends. There is no
      prefetching and no offline download. At most 2 requests run at once, each with a
      distinct User-Agent.
    - Tiles are cached on disk according to `Cache-Control`/`Expires`, kept 7 days when the
      server sends no caching headers, and revalidated with `If-None-Match`/`If-Modified-Since`.
      `no-cache` is never sent, and `no-store` is honoured.
    - "© OpenStreetMap contributors" is drawn on the map, with links to the copyright page and
      "Report a map issue".
    - The tile URL is configurable in Settings (https only), and the cache can be cleared there.
    - Places named by AI have no coordinates. The user checks the evidence and clicks
      "Place on map", which records `coordinate_source = user` and confirms the place.
      Ambiguous places are flagged.
    - There is no geocoder: Nominatim's policy rules out automatic bulk lookups.
- **M8 Distribution**: done, except checks on real Linux desktops.
  - Benchmarks at 10k/100k/1M and the tuning they led to: BENCHMARKS.md.
  - Security review and `cargo audit`: SECURITY.md (no vulnerabilities; one unsound transitive
    crate through Iced, documented).
  - Linux (Fedora 44 under WSL): all tests and clippy pass. The release app runs on Wayland and
    X11 with the GPU and CPU renderers.
  - Windows NSIS installer: verified with a silent install, launch, and an uninstall that leaves
    nothing behind.
  - `.deb` and AppImage built on Ubuntu 22.04: the `.deb` installs with apt on a clean Ubuntu
    24.04 with all libraries resolved, and the AppImage runs on Fedora 44.
  - Not verified: native GNOME/KDE sessions on real hardware (WSLg's GPU passthrough made wgpu
    fall back), the OS secret store on Linux desktops, code signing (no certificate), and
    rendering by eye (the test machine's screen was locked throughout).

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

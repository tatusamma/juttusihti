# Full Article Fetch Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A user-started "Read full article" action that loads the article in a hidden, throwaway Servo browser, keeps only the text and main picture, and shows the picture as a hero fade.

**Architecture:** A new helper binary `juttusihti-browser` embeds Servo 0.7 headless. It handles exactly one fetch per process: it reads a JSON request on stdin, uses a fresh profile directory that it deletes before exit, and writes a JSON outcome on stdout. A small `juttusihti-article` crate holds the protocol types and picture processing, with no Servo dependency. Core stores fetched text in a new `article_fulltext` table that feed refresh never touches, folds it into search and AI full-text, and enforces the cache limit. The app spawns the helper, kills it on timeout, and renders the result.

**Tech Stack:** Rust 2024, Servo 0.7 (`servo` crate), `adblock` (Brave's adblock-rust, MPL-2.0) with bundled EasyList + EasyPrivacy, vendored Mozilla Readability.js (Apache-2.0), vendored DuckDuckGo autoconsent bundle (MPL-2.0), `image` 0.25, Iced 0.14, rusqlite/FTS5.

**Spec:** `docs/superpowers/specs/2026-11-06-full-article-fetch-design.md`

## Global Constraints

- Servo comes from crates.io, `servo = "0.7"`. Raise workspace `rust-version` only if Servo requires it, and say so in the CHANGELOG.
- Servo's default user agent is used exactly as shipped. No code anywhere sets or changes a user agent for the browser helper, including on retry. There are no retries after a refusal.
- No network request happens unless the user pressed "Read full article". Block lists and consent rules are bundled at build time. Nothing is downloaded at runtime.
- Every fetch uses a new empty profile directory, which is deleted when the helper exits. The app deletes leftovers at startup.
- Consent (user decision, replaces reject-first): accept automatically (autoconsent `optIn`); all cookies are deleted with the profile after extraction. The user never clicks anything.
- The helper never bypasses a paywall. A paywalled article keeps the public text and is flagged `paywalled`.
- Settings keys and defaults: `fulltext_pictures` = `"1"` (on), `fulltext_cache_mb` = `500`.
- Automatic cache cleanup never touches articles with `is_saved = 1`. "Clear cache now" does clear them.
- Cache cleanup removes only the fetched text and picture, never the `articles` row.
- UI copy: button labels are `Read full article`, `Fetching…` and `Fetched`. The paywall note is `The rest of this article is for subscribers.` The keyboard shortcut is `a`.
- Hero fade fades into `palette.background` (dark `#1c1b1a`, light `#faf7f2`) and must render with the CPU renderer (tiny-skia).

## Review Focus

1. **Helper hangs or crashes mid-fetch.** The app kills it after 60 s and deletes its profile directory. The button returns to `Read full article` with a one-line failure note. Test: Task 6 `hung_helper_is_killed_and_profile_removed`.
2. **The user moves to another article while a fetch runs.** The result is saved for the original article, the preview stays on the article now open, and the original article shows `Fetched` when reopened. Test: Task 7 `result_for_other_article_does_not_replace_preview`.
3. **The picture is huge, undecodable (AVIF, SVG) or missing.** The text is still saved and the reader shows no picture, with no error. Test: Task 2 `undecodable_or_oversized_picture_is_dropped`.
4. **The article has no URL or a non-http(s) URL** (`javascript:`, `file:`). The button is disabled and the helper is never spawned. Test: Task 6 `non_http_url_is_rejected_before_spawn`.
5. **The cache limit is lowered below the current size, or the article was deleted mid-fetch.** Cleanup runs right away down to the new limit, Saved articles are kept even when that leaves the cache over the limit, and saving to a deleted article returns `NotFound` with the picture file removed. Tests: Task 4 `lowering_limit_evicts_oldest_unsaved_only` and `save_for_missing_article_is_not_found`.

---

## File Structure

| Path | Responsibility |
| --- | --- |
| `crates/article/` (new, `juttusihti-article`) | Protocol types `FetchRequest`/`FetchOutcome`, `process_picture`, `normalize_text`. No Servo. |
| `crates/browser/` (new, bin `juttusihti-browser`) | Servo driver: profile dir, blocking, consent, extraction, in-page picture fetch. |
| `crates/browser/assets/` | Vendored `Readability.js`, `autoconsent.js` (+ rules), `easylist.txt`, `easyprivacy.txt`, `extract.js`, `LICENSES.md` (versions + licences). |
| `crates/core/migrations/0006_fulltext.sql` | `article_fulltext` table and FTS rebuild over a view. |
| `crates/core/src/fulltext.rs` | Store functions for fetched text and the cache. |
| `crates/app/src/full_article.rs` | Spawning the helper, timeouts, startup sweep. |
| `crates/app/src/settings.rs`, `app.rs`, `style.rs`, `help.rs` | Settings section, reader button/states/hero, shortcut, help. |
| `Packager.toml`, `crates/app/Cargo.toml` (rpm), `.github/workflows/release.yml`, `packaging/release-windows.ps1`, README, CHANGELOG | Ship and document the second binary. |

---

### Task 1: Servo compatibility gate (STOP point)

**Files:**
- Create: `crates/browser/Cargo.toml`, `crates/browser/src/main.rs`, `crates/browser/src/engine.rs`, `crates/browser/assets/Readability.js`, `crates/browser/assets/extract.js`
- Modify: `Cargo.toml` (workspace members)
- Test: `crates/browser/tests/local_page.rs`

**Interfaces:**
- Produces: `engine::load_and_extract(url: &Url, profile_dir: &Path, timeout: Duration) -> Result<Extracted, EngineError>`, with `struct Extracted { main_status: Option<u16>, title: String, text: String, html_lang: Option<String>, picture_url: Option<String>, paywalled: bool, cookie_names: Vec<String> }` and `enum EngineError { Refused(u16), Timeout, NoArticle, Engine(String) }`.
- Temporary CLI: `juttusihti-browser --probe <url>` prints `Extracted` as JSON. Task 3 replaces it.

- [ ] **Step 1: Write the failing test.** `crates/browser/tests/local_page.rs::extracts_text_from_local_article` starts a `std::net::TcpListener` on 127.0.0.1 that serves `tests/fixtures/article.html`. The fixture has an `og:image`, three `<p>` paragraphs, a `Set-Cookie: probe=1` header and `"isAccessibleForFree": false` JSON-LD. The test asserts that `text` contains the second paragraph, that `picture_url` ends with `/hero.jpg`, that `paywalled == true` and that `cookie_names == ["probe"]`.
- [ ] **Step 2: Run** `cargo test -p juttusihti-browser --test local_page`. Expected: FAIL (crate/function missing).
- [ ] **Step 3: Implement `engine::load_and_extract`.** Build Servo with `ServoBuilder` and a `SoftwareRenderingContext` (no window), using a config/profile dir set to `profile_dir`. Create one `WebView` and wait for `WebViewDelegate::notify_load_status_changed(LoadStatus::Complete)`. Then call `WebView::evaluate_javascript` with `Readability.js` + `extract.js`. `extract.js` returns JSON: Readability `textContent`, the `og:image` / JSON-LD `image` / first `<img>` at least 400 px wide, JSON-LD `isAccessibleForFree === false` → `paywalled`, and `document.cookie` names. Take the main-document HTTP status from Servo if it exposes one. A status ≥ 400 gives `Refused`. A Readability result of null or under 200 chars gives `NoArticle`. Check names against doc.servo.org for 0.7. Do not touch the user agent.
- [ ] **Step 4: Run the test.** Expected: PASS. Note any extra system build dependencies Servo needed (clang, Python, GL/EGL on Linux) for Task 8.
- [ ] **Step 5: Probe real sites.** Take the three newest article URLs per feed from the live DB, read-only (`sqlite3` URI `?mode=ro`, path `%APPDATA%\Juttusihti\Juttusihti\data\juttusihti.db`), and run `--probe` on each, one at a time. Record per URL: status/outcome, text length, picture found, paywalled, cookie names.
- [ ] **Step 6: Apply the gate and report to the user.** The feature is built if at least one of IS and HS yields full article text from a free article. A paywalled HS result alone does not count for HS. If neither site qualifies, STOP the plan and report. Never change the user agent or retry differently after a refusal.
- [ ] **Step 7: Commit** `git add Cargo.toml Cargo.lock crates/browser && git commit -m "Add Servo browser helper and check it against IS and HS"`

### Task 2: Protocol and picture processing crate

**Files:**
- Create: `crates/article/Cargo.toml`, `crates/article/src/lib.rs`
- Test: in-file `#[cfg(test)]`, fixtures `crates/article/tests/fixtures/{photo.jpg,photo.webp,tiny.avif,big.png}`

**Interfaces:**
- Produces:
  ```rust
  pub struct FetchRequest { pub url: String, pub want_picture: bool, pub profile_dir: PathBuf, pub picture_path: PathBuf, pub timeout_secs: u32 }
  pub enum FetchOutcome { Fetched { text: String, paywalled: bool, picture: Option<PictureInfo> }, Refused { status: u16 }, Timeout, NoArticle, Failed { message: String } }
  pub struct PictureInfo { pub path: PathBuf, pub bytes: u64, pub width: u32, pub height: u32 }
  pub const MAX_PICTURE_INPUT_BYTES: usize = 8 * 1024 * 1024;
  pub const PICTURE_MAX_WIDTH: u32 = 1600;
  pub fn process_picture(input: &[u8], out: &Path) -> Option<PictureInfo>;
  pub fn normalize_text(raw: &str) -> String;
  ```
  All types derive `serde::{Serialize, Deserialize}` (`FetchOutcome` is internally tagged with `#[serde(tag = "kind")]`) plus `Debug, Clone, PartialEq`.

- [ ] **Step 1: Write the failing tests.**
  - `request_and_outcome_round_trip_json`: every `FetchOutcome` variant survives `serde_json` to and from JSON.
  - `jpeg_and_webp_are_resized_to_jpeg`: a 3000 px wide JPEG and a WebP each produce a JPEG ≤ 1600 px wide with the aspect ratio kept and `bytes` equal to the file size.
  - `undecodable_or_oversized_picture_is_dropped`: AVIF, SVG text and a 9 MB buffer return `None` and write no file.
  - `normalize_text_keeps_paragraphs`: `"  a\n\n\n b \n\nc  "` gives `"a\n\nb\n\nc"`.
- [ ] **Step 2: Run** `cargo test -p juttusihti-article`. Expected: FAIL.
- [ ] **Step 3: Implement.** `image` 0.25 with `jpeg`, `png`, `webp` and `gif` features, `Lanczos3` resize, JPEG quality 80. Reject before decoding when the input is over `MAX_PICTURE_INPUT_BYTES`.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "Add the article fetch protocol and picture processing"`

### Task 3: Complete the browser helper (privacy, blocking, consent, picture)

**Files:**
- Modify: `crates/browser/src/main.rs`, `crates/browser/src/engine.rs`
- Create: `crates/browser/src/blocker.rs`, `crates/browser/src/consent.rs`, `crates/browser/assets/{easylist.txt,easyprivacy.txt,autoconsent.js,LICENSES.md}`
- Test: `crates/browser/src/blocker.rs` (unit), `crates/browser/tests/local_page.rs`

**Interfaces:**
- Consumes: `juttusihti_article::{FetchRequest, FetchOutcome, process_picture, normalize_text}`, `engine::load_and_extract` (Task 1, now extended).
- Produces the helper contract used by Task 6. `juttusihti-browser` (no args) reads one `FetchRequest` JSON from stdin and writes exactly one `FetchOutcome` JSON line to stdout. Exit code is 0 for every outcome. Before exiting it deletes `profile_dir` recursively. The `--probe` flag is removed.
- `blocker::Blocker::bundled() -> Blocker`, `Blocker::should_block(&self, url: &str, source_url: &str, kind: ResourceKind) -> bool`.

- [ ] **Step 1: Write the failing tests.**
  - `blocker::tests::blocks_known_trackers_allows_article`: `https://www.google-analytics.com/analytics.js` from `https://www.is.fi/x` is blocked; `https://www.is.fi/x.html` and `https://images.sanoma-sndp.fi/a.jpg` are not.
  - `local_page::second_fetch_sees_no_state_from_first`: two helper runs with the same URL but different `profile_dir`. The page sets a cookie and `localStorage` and echoes both into the DOM. The second run's text contains `cookie=` and `storage=` with empty values. Both profile dirs no longer exist after the runs.
  - `local_page::picture_written_when_wanted`: with `want_picture: true` the outcome has `picture.path == picture_path` and the file exists. With `false` there is no file and the server log shows no request for `/hero.jpg`.
  - `local_page::service_worker_registration_is_refused`: a page that calls `navigator.serviceWorker.register` leaves no registration on a second run.
- [ ] **Step 2: Run** `cargo test -p juttusihti-browser`. Expected: FAIL.
- [ ] **Step 3: Implement.**
  - Blocking: `WebViewDelegate::load_web_resource` and `ServoDelegate::load_web_resource` call `Blocker::should_block`. Blocked loads are intercepted with an empty 204 response. The `ServoDelegate` path blocks every non-WebView load (service workers).
  - Consent: after load completes, inject `autoconsent.js` with `autoAction: "optOut"`. If it reports no opt-out, run once more with `"optIn"`. Wait up to 5 s for a navigation it triggers, then extract.
  - Picture: when `want_picture`, run an in-page `fetch(picture_url)` that returns base64. This goes through the same session and blocker. Cap it at `MAX_PICTURE_INPUT_BYTES`, then pass it to `process_picture(…, &picture_path)`.
  - Text: pass it through `normalize_text`.
  - Timeout: `timeout_secs` from the request (the app sends 45).
  - Record each vendored asset's version, source URL and licence in `assets/LICENSES.md`.
- [ ] **Step 4: Run.** Expected: PASS. Re-run Task 1's real-site probes through the new stdin interface and confirm the gate result still holds.
- [ ] **Step 5: Commit** `git commit -m "Block trackers, dismiss consent banners and wipe the profile in the browser helper"`

### Task 4: Store fetched text, search, AI and cache in core

**Files:**
- Create: `crates/core/migrations/0006_fulltext.sql`, `crates/core/src/fulltext.rs`
- Modify: `crates/core/src/db.rs:14-19` (add migration), `crates/core/src/lib.rs`, `crates/core/src/store.rs:269-279` (`article_text`), `crates/core/src/analysis.rs:474-512`, `crates/core/src/search.rs:41,525`
- Test: `crates/core/tests/fulltext.rs`

**Interfaces:**
- Produces (methods on `Store`, in `fulltext.rs` as `impl Store`):
  ```rust
  pub struct Fulltext { pub article_id: ArticleId, pub text: String, pub paywalled: bool, pub picture_path: Option<String>, pub fetched_at: i64 }
  pub fn save_fulltext(&self, id: ArticleId, text: &str, paywalled: bool, picture: Option<(&str, u64)>) -> Result<()>; // Error::NotFound if article gone
  pub fn fulltext(&self, id: ArticleId) -> Result<Option<Fulltext>>;
  pub fn fulltext_cache_bytes(&self) -> Result<u64>;
  pub fn enforce_fulltext_limit(&self, limit_bytes: u64) -> Result<Vec<String>>; // removed picture paths
  pub fn clear_fulltext(&self) -> Result<Vec<String>>;                            // removed picture paths, saved included
  pub fn fulltext_picture_paths(&self) -> Result<Vec<String>>;                    // for orphan sweep
  ```
- Schema: `article_fulltext(article_id INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE, text TEXT NOT NULL, paywalled INTEGER NOT NULL CHECK (paywalled IN (0,1)), picture_path TEXT, text_bytes INTEGER NOT NULL, picture_bytes INTEGER NOT NULL DEFAULT 0, fetched_at INTEGER NOT NULL)`. `picture_path` is relative to the data dir.
- FTS: drop and recreate `articles_fts` with a fifth column `fetched_text`, external content `articles_search`, a view of `articles LEFT JOIN article_fulltext`. Recreate the `articles_*` triggers to include `fetched_text`, add insert/update/delete triggers on `article_fulltext`, then run `INSERT INTO articles_fts(articles_fts) VALUES('rebuild')`. Every FTS `'delete'` must pass exactly the values currently indexed, so the delete paths through `ON DELETE CASCADE` need care. The integrity tests below decide.
- Search: `Column::Content` maps to the FTS5 column set `{content_text fetched_text}`. BM25 weights become `bm25(articles_fts, 10.0, 3.0, 1.0, 2.0, 1.0)`.
- Analysis: the body source is `COALESCE(NULLIF(ft.text,''), a.content_text, '')`. For `Depth::FullText`, the hash passed to `cache_key`/`clickbait_key` is `content_hash` + `|ft:` + sha256 of the fetched text when one exists, so cached results from before the fetch are not reused.

- [ ] **Step 1: Write the failing tests** in `crates/core/tests/fulltext.rs`. After each one, run `INSERT INTO articles_fts(articles_fts, rank) VALUES('integrity-check', 1)` and assert it succeeds.
  - `fetched_text_is_searchable_and_survives_refresh`: save fulltext with the word `kuntavaalit`. Search `kuntavaalit` and `content:kuntavaalit` finds it. Re-`ingest` the entry with a changed summary; `fulltext()` is unchanged and the search still finds it.
  - `article_text_prefers_fetched`.
  - `fts_consistent_after_article_delete_retention_and_feed_removal`: articles with fetched text are removed by `apply_retention`, by `remove_feed`, and by direct delete.
  - `lowering_limit_evicts_oldest_unsaved_only`: three fetched articles of 100 bytes each, the oldest one `is_saved`. `enforce_fulltext_limit(150)` removes the middle one and then the newest only if the total is still over 150. The saved article is kept even though that leaves the total over the limit. Returned paths match the evicted pictures. The `articles` rows remain.
  - `clear_removes_saved_too`.
  - `save_for_missing_article_is_not_found`.
  - In `analysis.rs` tests, `fulltext_depth_uses_fetched_text_and_new_cache_key`.
- [ ] **Step 2: Run** `cargo test -p juttusihti-core`. Expected: the new tests FAIL and the old ones pass.
- [ ] **Step 3: Implement the migration and functions.**
- [ ] **Step 4: Run** `cargo test -p juttusihti-core`. Expected: all PASS, including the existing search tests.
- [ ] **Step 5: Commit** `git commit -m "Store fetched article text apart from the feed and search it"`

### Task 5: Settings section "Full articles"

**Files:**
- Modify: `crates/app/src/settings.rs`
- Test: `crates/app/src/settings.rs` tests module (line 342)

**Interfaces:**
- Produces: `KEY_FULLTEXT_PICTURES = "fulltext_pictures"`, `KEY_FULLTEXT_CACHE_MB = "fulltext_cache_mb"`, `DEFAULT_FULLTEXT_CACHE_MB: u32 = 500`. `Values` gains `fulltext_pictures: bool` (default `true`) and `fulltext_cache_mb: u32` (default 500). The new message `Msg::ClearFulltextCache` is handled by the parent app (Task 7), which runs `clear_fulltext` and deletes the files.
- Validation: cache MB is an integer from 10 to 100000. Otherwise the error is `Cache size must be between 10 and 100000 MB`.

- [ ] **Step 1: Extend tests.** In `reads_defaults_and_stored_values`, check that the defaults are `true`/500 and that stored `"0"`/`"250"` read back. In `validates_form`, `"5"` and `"abc"` are rejected with the exact message and `"250"` is accepted.
- [ ] **Step 2: Run** `cargo test -p juttusihti settings`. Expected: FAIL.
- [ ] **Step 3: Implement.** Add a "Full articles" section with a "Download pictures" checkbox, a "Cache size limit (MB)" input, a "Clear cache now" quiet button and a line showing the current cache size (`fulltext_cache_bytes`, formatted in MB). Saving a lower limit triggers `enforce_fulltext_limit` immediately via the parent.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "Add full article settings: pictures and cache size"`

### Task 6: Run the helper from the app

**Files:**
- Create: `crates/app/src/full_article.rs`
- Modify: `crates/app/src/main.rs` (module and startup sweep), `crates/app/Cargo.toml` (`juttusihti-article`, `tokio` `process` feature)
- Test: `crates/app/src/full_article.rs` tests, using a fake helper built from `crates/app/tests/fake_helper.rs` as an `[[example]]`

**Interfaces:**
- Consumes: the Task 3 helper contract and `juttusihti_article::{FetchRequest, FetchOutcome}`.
- Produces:
  ```rust
  pub const FETCH_TIMEOUT: Duration = Duration::from_secs(60); // helper gets timeout_secs = 45
  pub fn helper_path() -> PathBuf; // JUTTUSIHTI_BROWSER env var, else sibling of current_exe: juttusihti-browser(.exe)
  pub fn check_url(url: Option<&str>) -> Result<Url, &'static str>; // http/https only
  pub async fn fetch(helper: PathBuf, data_dir: PathBuf, article_id: ArticleId, url: Url, want_picture: bool) -> FetchOutcome;
  pub fn startup_sweep(data_dir: &Path, store: &Store, limit_bytes: u64);
  ```
- Layout under the data dir: profiles in `browser-profiles/<article_id>-<random>/`, pictures in `fulltext/<article_id>.jpg`. Paths stored in the DB are relative (`fulltext/<id>.jpg`).
- `fetch` maps a missing binary, a spawn error, unparsable stdout or a non-zero exit to `Failed { message }`. On timeout it kills the child, returns `Timeout` and removes the profile dir. It always removes the profile dir afterwards as a backstop.
- `startup_sweep` empties `browser-profiles/`, runs `enforce_fulltext_limit`, deletes the returned pictures, and deletes any file in `fulltext/` that is not in `fulltext_picture_paths()`.

- [ ] **Step 1: Write the failing tests.**
  - `non_http_url_is_rejected_before_spawn`: `check_url` rejects `None`, `javascript:alert(1)`, `file:///c:/x` and `ftp://x`, and accepts `https://www.hs.fi/a.html`.
  - `fake_helper_outcome_is_returned`: the fake helper echoes a `Fetched` outcome and `fetch` returns it.
  - `hung_helper_is_killed_and_profile_removed`: the fake helper sleeps forever. With the timeout overridden to 2 s in the test, `fetch` returns `Timeout` within 4 s and the profile dir does not exist.
  - `missing_helper_is_failed`.
  - `startup_sweep_removes_leftovers_and_orphans`.
- [ ] **Step 2: Run** `cargo test -p juttusihti full_article`. Expected: FAIL.
- [ ] **Step 3: Implement.** Use `tokio::process::Command` with `kill_on_drop(true)`, write the stdin JSON and read one stdout line. Make the timeout a parameter of an inner `fetch_with_timeout` so tests can shorten it. On Windows, spawn with `CREATE_NO_WINDOW`.
- [ ] **Step 4: Run.** Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "Run the browser helper from the app with a timeout and cleanup"`

### Task 7: Reader button, states and hero fade

**Files:**
- Modify: `crates/app/src/app.rs` (`Preview` at 131, `Message`, `on_key` at 1148, `view_preview` at 1577), `crates/app/src/style.rs` (hero gradient), `crates/app/Cargo.toml` (`image` gains `jpeg`), `crates/app/src/help.rs:10` (shortcut row, array length 11)
- Test: `crates/app/src/app.rs` tests

**Interfaces:**
- Consumes: `full_article::{fetch, check_url, helper_path}`, `Store::{save_fulltext, fulltext, enforce_fulltext_limit}`, and `settings::Values::{fulltext_pictures, fulltext_cache_mb}`.
- Produces:
  ```rust
  enum FullState { Idle, Busy, Fetched, Failed(String) }        // per preview
  Message::FetchFullArticle, Message::FullArticleDone(ArticleId, FetchOutcome), Message::FulltextSaved(ArticleId, Result<(), String>)
  fn outcome_note(o: &FetchOutcome) -> Option<String>;          // failure copy below
  ```
- `App` keeps `fetching: Option<ArticleId>`. While it is `Some`, every "Read full article" button is disabled. `Preview` gains `fulltext: Option<Fulltext>`, `picture: Option<image::Handle>` and `full: FullState`.
- Failure copy (exact):
  - `Refused{status}` → `The site refused the request (HTTP {status}). Open it in the browser to read it.`
  - `Timeout` → `The site took too long to answer.`
  - `NoArticle` → `Couldn't find the article text on the page.`
  - `Failed{message}` → `Fetching failed: {message}`
- Hero: shown when `picture` is `Some`. It is a `stack!` of the image (`ContentFit::Cover`, full pane width, height 260) and a container with a linear gradient from `palette.background` at alpha 0.15 at the top, through 0.55 at 55 %, to 1.0 at the bottom. The meta line, headline and actions overlap its lower 130 px. It scrolls with the article. The JPEG is decoded to RGBA with `image` and given to `Handle::from_rgba`.

- [ ] **Step 1: Write the failing tests.**
  - `outcome_note_copy`: covers each variant, with the exact strings above.
  - `result_for_other_article_does_not_replace_preview`: with the preview on article B, `FullArticleDone(A, Fetched…)` leaves the preview body unchanged and clears `fetching`. It issues a `save_fulltext` task for A.
  - `fetch_disabled_while_another_runs`.
  - `paywalled_fetch_shows_note`: the preview body ends with the paywall note.
- [ ] **Step 2: Run** `cargo test -p juttusihti`. Expected: FAIL.
- [ ] **Step 3: Implement.**
  - Put the button after "Open in browser". Its label is `Read full article` when idle, `Fetching…` while busy (disabled) and `Fetched` once done (disabled, `quiet_active`). Disable it when `check_url` fails.
  - Shortcut `a` sends `FetchFullArticle`.
  - After a `Fetched` outcome: save, run `enforce_fulltext_limit` with `fulltext_cache_mb * 1_048_576`, delete the returned files, and reload the preview if it is still that article.
  - On `NotFound`, delete the picture file.
  - With pictures turned off, send `want_picture: false`.
  - Handle the `Msg::ClearFulltextCache` message from Task 5.
- [ ] **Step 4: Run** `cargo test -p juttusihti`. Expected: PASS.
- [ ] **Step 5: Manual check.** Following the user's standing rule, drive the app only through its own window handle with a copy of the DB in `JUTTUSIHTI_DATA_DIR`, never with screen clicks. Fetch one IS article in the dark and light themes and with the CPU renderer (`ICED_BACKEND=tiny-skia`). Take screenshots of the hero fade for the user.
- [ ] **Step 6: Commit** `git commit -m "Read full articles in the reader with a faded hero picture"`

### Task 8: Packaging, docs and release

**Files:**
- Modify: `Packager.toml` (second `[[binaries]]`, `beforePackagingCommand` builds `-p juttusihti -p juttusihti-browser`), `crates/app/Cargo.toml` rpm `assets`, `.github/workflows/release.yml:75`, `.github/workflows/ci.yml` (Servo build deps for ubuntu-22.04 and windows), `packaging/release-windows.ps1:124` (sign both exes, include both in the zip), the Linux tarball/AppImage steps, `README.md` (Features, Privacy, Build from source deps), `CHANGELOG.md` (Unreleased), `crates/app/src/help.rs` (Reader section text)
- Test: CI green on both OSes. Locally: `cargo build --release -p juttusihti -p juttusihti-browser` and `cargo packager --release -f nsis`. Install, then check that `juttusihti-browser.exe` sits next to `juttusihti.exe` and that both are signed (`signtool verify /pa`) using the user's `-SignCommand`.

- [ ] **Step 1: Packaging.** Ship the helper in every format and verify it with the local NSIS build.
- [ ] **Step 2: CI.** Add the Servo build dependencies Task 1 recorded and confirm `cargo test --workspace --locked` passes on both runners.
- [ ] **Step 3: Docs.** Add a README feature bullet ("Full articles: one click loads the article in a throwaway private browser…"), a privacy paragraph listing what is blocked and that the profile is deleted, and the build deps. Add the Help screen text and shortcut row, a CHANGELOG entry, and a link to `crates/browser/assets/LICENSES.md` in the README licence section.
- [ ] **Step 4: Commit** `git commit -m "Package the browser helper and document full articles"`

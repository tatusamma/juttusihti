# Full article fetch: read the whole story, privately

Date: 2026-11-06. Applies to the workspace at 0.3.0 (`crates/app`, `crates/core`, `crates/fetch`).

## Why

The user's feeds (Ilta-Sanomat and Helsingin Sanomat, 847 articles in the live database) send a
headline and about 100 characters of teaser. `articles.content_text` is empty for every one of them.
When a headline is interesting, the only way to read more today is "Open in browser", which hands
the visit to a normal browser profile where the news site and its trackers recognise the user.

The user wants to press a button and read the full article inside Juttusihti, with nobody able to
follow them afterwards: no clicking cookie popups, every cookie gone once the article is saved.
If the article has a picture, it should look good: faded in behind the headline.

## What a plain HTTP fetch showed (2026-11-06)

A test fetch of six recent articles with no cookies and no browser:

- IS and HS returned **403 Forbidden** to most plain requests; they refuse non-browser clients.
- One HS article came through. It set no cookies, had an `og:image`, and carried paywall markers
  (`isAccessibleForFree: false`, "Tilaajille").

So the obstacle on these sites is not the cookie popup, it is that they serve real browsers only.
Disguising a plain HTTP client as a browser was rejected (see anti-goals). The decision is to **be**
a real browser: Servo, embedded and hidden.

## Goals

- A **Read full article** action on any article, started only by the user.
- The fetched text and main picture are kept locally and readable offline.
- Every fetch is a stranger's visit: a brand-new empty browser profile, deleted afterwards with all
  its cookies and storage. Known trackers and ads are blocked while the page loads. Cookie consent
  is handled automatically with no user clicks.
- The main picture is shown in the **hero fade** style (below), and can be turned off.
- A cache with a user-adjustable size limit, cleaned automatically.
- Works for most sources, not only IS/HS. Windows and Linux.

## Non-goals

- No automatic or background fetching of articles. Feed refresh is unchanged.
- No inline article images beyond the one main picture; no video, embeds, comments or galleries.
- No signing in to news sites and no subscriber access.
- No change to "Open in browser", which keeps opening the system browser.

## Anti-goals (a failure even if it works)

- **Never disguise the client.** No spoofed user agents on a non-browser client, no fingerprint
  tricks, no attempts to look less automated, no bot-check or CAPTCHA solving. If a site refuses
  Servo, the fetch fails politely.
- **Never bypass a paywall.** Subscriber-only articles keep only what the site shows publicly.
- Any state that survives between two fetches (cookies, local storage, cache, service workers,
  HSTS/alt-svc beyond what is unavoidable) is a privacy bug.
- A crash or hang in the browser engine must not take down or freeze the app.
- Any network request the user did not start.

## Decisions made with the user

### Engine: Servo 0.7

Servo, from crates.io (`servo` 0.7, <https://github.com/servo/servo>), embedded headless. The user
chose it over WebView2 (Microsoft, considered spyware by the user), CEF/Chromium (adds 150–200 MB,
heavy memory) and driving an installed Firefox. It is Rust, has no telemetry, and is a genuine
browser engine, so the visit is honest.

Known risk, accepted by the user: Servo is pre-1.0 with monthly breaking API changes, and its
compatibility with heavy news sites is unproven. **The first implementation step is a compatibility
check against real IS and HS article URLs from the user's database** (does the site serve Servo, does
the article render, can the text and picture be extracted, which cookies appear). Report the result
before building the rest. Do not work around a refusal.

Gate (decided with the user): build the full feature if **at least one** of IS and HS yields full
article text (for HS, a subscriber-only article that gives its public opening paragraphs counts as
a paywalled result, not as a working site; at least one free HS article must come through for HS
to count). If **neither** site works, stop and bring it back to the user.

Browser identity (decided with the user): use Servo's default user agent exactly as Servo ships it,
even though it carries a Firefox compatibility token like most browsers do. Never change it, and
never change it as a retry after a refusal.

### The flow

1. The article pane's action row gets **Read full article** next to "Open in browser".
2. Pressing it shows a busy state on the button. One fetch at a time.
3. Servo starts with a new empty profile in a temporary directory, loads the article URL with
   tracker and ad blocking active, handles the cookie consent banner, waits for the article to
   render, extracts the article text and identifies the main picture, and downloads that picture in
   the same session (so it is subject to the same blocking and dies with the same profile).
4. The profile directory is deleted. Nothing from the session is kept except the extracted text and
   the picture.
5. The full text replaces the teaser in the reader; the button becomes **Fetched**. Reopening the
   article is instant and offline.
6. On failure (refused, timeout, nothing extractable) the reader shows a one-line note saying why,
   and the teaser and "Open in browser" stay as they were.

### Picture: hero fade (option A)

The main picture spans the top of the reading pane at full pane width. It is clear at the top edge
and fades through a gradient into the pane background under the headline. The source line, headline
and action buttons sit over the lower, faded part. It scrolls away with the article. Mockup:
`.superpowers/brainstorm/1487-1793993902/content/picture-style.html` (option A).

In the dark theme it fades into `#1c1b1a`; in the light theme into the paper background `#faf7f2`
(see `crates/app/src/style.rs`). Text over the picture must stay readable on any photo, so the top
part of the gradient also dims slightly. With no picture, or pictures turned off, the reader looks
exactly as it does today.

### Settings (new "Full articles" section in the Settings screen)

- **Download pictures**: on by default. Off means text only; the picture is never requested.
- **Cache size limit**: adjustable, in MB.
- **Clear cache now**: removes all fetched text and pictures.

### Tracker and consent rules ship with the app

The tracker block list and the cookie-consent rules are pinned copies bundled at build time and
updated only with Juttusihti releases. No download of rules at runtime. A user-pressed "Update
rules" button may come later if banners start getting through; it is out of scope now. Record the
pinned versions and licences so the release process can refresh them.

### Cache

Fetched text and pictures count toward the limit. When the cache is over the limit, the oldest
fetched articles lose their fetched content first. Cleanup runs at startup and after each fetch.

## Calls made by me (not discussed in detail; the user approved the summary)

- Cookie consent: **reject** when the banner offers a reject or "only necessary" choice; accept only
  when it does not. The user asked for "accept", but since everything is deleted afterwards either
  works, and rejecting means less tracking during the visit. Always without user clicks.
- Default cache limit **500 MB**.
- Articles starred with **Save** are never cleaned automatically ("Clear cache now" still clears them).
- Cache cleanup removes the *fetched* text and picture, never the article itself; the article falls
  back to its feed teaser and can be fetched again.
- Fetched text feeds full-text search and the existing "Archived article text" AI analysis depth
  (`Depth::FullText`), which today is empty for these feeds.
- Paywalled articles: keep whatever the public page shows and say in the reader that the rest is
  for subscribers.

## Constraints

- Feed refresh rewrites `articles.content` when the feed's content hash changes
  (`crates/core/src/store.rs`, the `UPDATE articles SET ... content = ?5` upsert). Fetched text must
  therefore live where refresh cannot overwrite it (a new table via a new migration
  `0006_...sql`), and search and analysis must be taught to read it.
- The app uses Iced 0.14 with `image-without-codecs` and only the PNG codec of `image` 0.25. News
  pictures are JPEG/WebP/AVIF, so either add decoders or re-encode on download. Downscale on download
  (e.g. to the pane's maximum useful width) to keep the cache small.
- The app supports a CPU renderer for low memory (`Values::cpu_renderer`). The fade must work there.
- Settings live in the `settings` key/value table, with keys and defaults in
  `crates/app/src/settings.rs`.
- Releases: signed Windows installer and portable zip, .deb, .rpm, AppImage, tarball
  (`packaging/`, `RELEASING.md`). Servo must build and ship in all of them; expect larger binaries
  and longer CI builds, and document new build dependencies in the README.
- Keyboard shortcuts: `f` is taken (focus search). Any shortcut for this action is the builder's
  choice, and must be listed in Help (F1).
- Help screen, README features list and CHANGELOG get an entry.

## Left to the builder

- Where Servo runs. A separate helper process (own binary, started per fetch or kept idle, talking
  to the app over stdin/stdout) is the obvious way to meet the "a crash must not take down the app"
  anti-goal and to guarantee all session state dies with it. In-process is acceptable only if those
  guarantees still hold.
- Which crate the code lives in (`crates/fetch` or a new crate).
- How tracker and ad blocking hooks into Servo's network layer, and which block list is used
  (licence must be compatible with distribution under MIT OR Apache-2.0).
- How consent banners are recognised and dismissed. A maintained rule set for common consent
  platforms is preferred over hand-written per-site rules; a few per-site rules are fine where needed.
- Article extraction (readability-style, in page or on the rendered DOM in Rust), and how the main
  picture is chosen (`og:image`, schema.org `image`, first large article image).
- Timeouts, the "done loading" signal, maximum page and picture sizes.
- Storage schema for fetched text, pictures (files under the data dir or BLOBs), fetch status and
  sizes.
- Exact UI states and wording for busy, fetched, failed and paywalled.
- Tests: extraction and cache cleanup should be unit-tested on saved HTML; the Servo path needs at
  least one test that loads a local page and checks that no cookie or storage survives.

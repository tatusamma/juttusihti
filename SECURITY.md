# Security

## Reporting a vulnerability

Please do not open a public issue. Report it privately through GitHub:
**Security → Report a vulnerability** on
[the repository](https://github.com/tero-k/juttusihti/security/advisories/new). Include the
version, your OS, and steps to reproduce. Fixes are released as a new version, and the
advisory is published once a fix is out.

Security fixes go to the latest release only.

Release files can be checked against `SHA256SUMS-*.txt` on the release, and the Linux packages
with `gh attestation verify` (see README). Windows files are Authenticode-signed.

## Security review

Review date: 2026-10-25. Scope: all crates at the commit that adds this file.

### Threat model

Untrusted inputs:
- Feed documents and the web pages used for feed discovery (any server the user subscribes to,
  plus any server those redirect to).
- OPML files.
- AI provider responses.
- Map tiles.
- Article web pages and their pictures, loaded by the full-article helper (Servo) only when the
  user presses "Read full article".
- Fact check evidence: the provider's web search results, Wikipedia search replies and Google
  Fact Check Tools replies, requested only after the user consents to a check.
- CSV consumers (spreadsheets that open our exports).

Valuables:
- the user's API keys
- the research database
- the user's reading and research privacy
- money spent on paid APIs

### Controls and where they are tested

| Risk | Control | Test |
|---|---|---|
| SQL injection | All values are bound parameters. Dynamic SQL interpolates only constant column lists and `?` placeholders. | `search::tests`, `store.rs` |
| FTS5 query injection | Query text is parsed into an AST; every term is re-emitted quoted; length and term-count caps. | `neutralizes_operator_injection`, `rejects_bad_queries` |
| Arbitrary SQL from AI or users in analytics | DuckDB runs only fixed report queries; no free-form SQL entry point. | — |
| DuckDB extension / file / network abuse | Autoload and autoinstall off, external access off, community extensions off, memory and threads bounded, `lock_configuration`. | `locked_connection_blocks_extensions_files_and_reconfiguration` |
| Malicious feed content (HTML/script) | Content is reduced to plain text for display; HTML is never rendered or executed. | `html_to_text_strips_tags_scripts_and_entities` |
| Feed content that crashes the parser | Feeds are parsed before the database is locked, so a panic fails that feed and leaves the database usable. | `a_parser_panic_fails_only_that_feed` |
| `javascript:` / `file:` links | Only http(s) URLs are stored, opened in the browser, followed as redirects, or imported from OPML. | `parses_atom_and_drops_unsafe_links`, OPML tests, `refuses_non_web_urls` |
| Oversized or hostile documents | Size limits on feeds (16 MB), OPML (8 MB), stored fields (512 KB), AI responses (4 MB), tiles (1 MB). Limits are checked after decompression. | `rejects_oversized_bodies`, `rejects_non_images_and_oversized_tiles` |
| XML entity expansion / XXE | feed-rs (quick-xml) does not resolve external entities or expand DTD entities. | — (library property) |
| Prompt injection in articles | Article text is serialized as JSON with `<` and `>` escaped inside a data block. Instructions say the text is material, not commands. Output is constrained to a JSON schema. | `injected_text_cannot_close_the_data_block` |
| Model output abuse | Output is validated again locally: only ids from the request are accepted, string lengths are bounded, values must come from the allowed enums, entities must occur in the article, evidence quotes must appear in the article text, and summary points need citations. Nothing is executed, and results are stored only as suggestions. | `extraction_validation_drops_unknown_ungrounded_and_oversized`, `invalid_and_injected_output_is_discarded` |
| Model-id path injection (Gemini URL) | Model ids are restricted to `[A-Za-z0-9._-]`, at most 128 characters. | `model_ids_are_restricted` |
| API key exposure | Keys are kept in the OS secret store, never in the database or settings. They are sent only in auth headers. `Endpoint`'s Debug output redacts the key. Provider requests do not follow redirects. | `endpoint_debug_hides_key`, `full_run_on_every_provider` |
| ChatGPT sign-in | Authorization Code + PKCE (S256) with fresh `state` and `nonce` per attempt. The loopback listener binds 127.0.0.1 only and ignores callbacks with another `state`. The ID token's issuer, audience, expiry and nonce are checked; its signature is not, because it arrives directly from the token endpoint over TLS (OpenID Connect Core §3.1.3.7). The plan scope must be granted, and a returning sign-in must be the same account. Only the refresh token is stored (secret store); refreshes are serialized and the rotated token is saved at once; dead tokens are cleared; sign-out revokes at OpenAI. A run approved for the plan cannot run on an API key or the reverse; the one exception is a clickbait check the user chose to pay with API credit, which is the only request sent with the key on such a run. | `chatgpt_sign_in_registers_then_returns`, `chatgpt_plan_run_streams_and_has_no_api_cost`, `chatgpt_refresh_failure_signs_out_and_counting_is_unsupported`, `billing_must_match_the_approved_session`, `a_split_run_sends_the_check_on_the_key_and_the_rewrites_on_the_plan`, `a_split_run_needs_the_key_for_its_check`, `id_token_claims_are_checked` |
| Unexpected spending | No AI call without an estimate, an approval step and explicit consent. A worst-case budget reservation is made before every call. Unknown prices are refused unless the user accepted them. A request cancelled mid-flight is charged at its worst case. Everything is recorded in the ledger. | `budget.rs`, `runner.rs` tests |
| Data sent to providers | Sending requires consent naming the provider and the content depth. OpenAI requests use `store: false`. Provider token counting is used only where documented as sending data. | `full_run_on_every_provider` |
| Map privacy | Tiles are opt-in, with a notice, and only viewport tiles are fetched. The tile URL must be https. | `tiles.rs` tests |
| CSV formula injection | Cells starting with `= + - @ tab CR` are prefixed with `'`. | `quotes_escapes_and_guards_formulas` |
| Data loss | AI never modifies source article columns. Retention never deletes saved, annotated or researched articles and has a 7-day minimum. Filtering is a view, never a deletion. | `retention_keeps_saved_and_researched_articles`, `hiding_dismissed_articles_is_reversible_and_needs_confirmation` |
| Path traversal | Cache paths are built from integers only; files are chosen through OS dialogs. | — |
| Hostile article pages (full-article helper) | Servo runs in a separate helper process, one per click, killed after 60 s, so a crash or hang cannot take the app down. The helper writes exactly one JSON line to a private copy of stdout; anything a page prints goes to stderr. The app accepts the picture only at the path it asked for, and deletes files only inside its own cache folders. Only http(s) article and picture URLs are loaded, the picture input is capped at 8 MB and re-encoded to JPEG. Extracted text is plain text; no page HTML is ever rendered by the app. | `page_output_cannot_reach_stdout`, `hanging_page_times_out_and_profile_is_removed`, `hung_helper_is_killed_and_profile_removed`, `foreign_or_missing_picture_is_dropped`, `local_file_picture_is_never_read`, `non_web_scheme_picture_is_never_read`, `non_http_url_is_rejected_before_spawn`, `delete_guard_rejects_unsafe_paths` |
| Tracking across full-article fetches | Every fetch gets a brand-new profile that is deleted afterwards (also after a crash or timeout; leftovers are cleared at startup). Service workers are refused. EasyList and EasyPrivacy block known trackers; the lists and consent rules ship with the app and are never downloaded at run time. Servo's own user agent is used unchanged. | `second_fetch_sees_no_state_from_first`, `service_worker_registration_is_refused`, `startup_sweep_removes_leftovers_and_orphans` |
| Fact check: prompt injection and invented evidence | The article, search results, published fact checks and Wikipedia extracts go to the model as escaped JSON in a data block, marked as data, not instructions. Claim quotes must appear in the article; cited URLs must have been gathered in this run, be http(s) and not on the article's own site; Wikipedia (also behind a Gemini redirect) cannot decide a verdict alone. A verdict without such a source is downgraded to Unverified. All strings and lists are bounded; links open only on click. | `extraction_drops_invented_quotes`, `cited_urls_must_come_from_gathered_results`, `invented_only_sources_downgrade_to_unverified`, `wikipedia_found_by_search_cannot_decide_alone`, `a_wikipedia_page_behind_a_redirect_is_background_only` |
| Fact check: Google key and lookups | The Google Fact Check Tools key is kept in the OS secret store and sent only to Google. Lookup errors are built without the request URL, which holds the key. Google and Wikipedia receive only a short search phrase per claim, never the article, with a generic Juttusihti User-Agent; replies are capped at 1 MB with no redirects. Without a key, nothing is sent to Google. | `errors_do_not_contain_the_key`, `no_key_sends_nothing`, `sends_generic_user_agent` |
| Fact check: spending | Nothing is sent before the consent panel's `Check with <provider>`, which names every recipient and shows the estimate. Each check has its own limit; every call reserves its worst case first and later claims are marked Not checked when they no longer fit. Usage goes to the ledger. | `limit_marks_remaining_claims_not_checked`, `too_low_limit_stops_before_extraction`, `cancel_saves_nothing` |
| Paywall bypass or paid clicks | Content a page hides, and parts its structured data marks as paid, are never read. Consent-or-pay walls get only the free "accept" choice; a priced choice is never clicked. | `hidden_and_paid_parts_are_never_read`, `priced_accept_choice_is_never_clicked`, `consent_or_pay_wall_is_accepted_for_free` |

### Accepted risks and follow-ups

1. **Feeds can point at the local network.** A subscribed feed, an OPML import or a redirect
   can make the app send GET requests to private addresses (for example a router's web page).
   Responses are only parsed as feeds and never shown raw, so data is not leaked. Blocking
   private ranges would break legitimate intranet feeds. Follow-up: warn on import when OPML
   contains private or loopback hosts.
2. **The PNG decoder handles untrusted tiles.** Decoding uses the `image` crate with its default
   allocation limits; only PNG is enabled.
3. **The local database is not encrypted.** It lives in the user's profile directory and relies
   on OS account protection.
4. **Accepting cookie banners.** The helper accepts consent banners so pages show their text.
   The site and the trackers that are not blocked see that one visit as consenting; the cookies
   are deleted with the profile, so visits cannot be linked. The site always sees the user's IP
   address.
5. **Dependency advisories:** see the next section.

### Dependency audit (`cargo audit`, RustSec database of 2026-11-08)

**App workspace** (`Cargo.lock`, 654 crates): no vulnerabilities. `arrow` was updated to 60.1.0
for RUSTSEC-2026-0442 (out-of-bounds read on malformed RunEndEncoded arrays). Warnings, all from
transitive dependencies:

| Advisory | Crate | Kind | Path | Assessment |
|---|---|---|---|---|
| RUSTSEC-2026-0253 | lru 0.16.4 | unsound (use-after-free if a panic happens inside `LruCache::pop`) | iced_wgpu → cryoglyph | Fixed only in lru ≥ 0.18.2, which Iced 0.14 cannot use without a fork. It is reachable only through the GPU text cache and is not attacker-triggerable. Track Iced releases; the CPU renderer setting avoids this path entirely. |
| — | async-executor 1.14.0 | yanked | iced_winit → mundy → zbus | Linux D-Bus stack only. Picked up when Iced or zbus updates. |
| RUSTSEC-2026-0192 | ttf-parser 0.25.1 | unmaintained | cosmic-text → fontdb | Parses only bundled and system fonts. No replacement available. |
| RUSTSEC-2026-0370/0371/0372 | zvariant, zbus_names, zvariant_derive | unmaintained | Linux D-Bus stack (winit/keyring/rfd) | No replacement available. Re-check when the zbus ecosystem moves. |
| RUSTSEC-2026-0441 | pin-utils 0.1.1 | unmaintained | iced_futures → wasmtimer | Compile-time macros only. Goes away when Iced drops it. |
| RUSTSEC-2024-0436 | paste 1.0.15 | unmaintained | build-time proc macro | No runtime exposure. |

**Full-article helper** (`crates/browser/Cargo.lock`, 840 crates, mostly Servo 0.7). Not part of
the CI audit job, because of the finding below; re-run it by hand in `crates/browser`.

| Advisory | Crate | Kind | Path | Assessment |
|---|---|---|---|---|
| RUSTSEC-2023-0071 | rsa 0.10.0-rc.18 | **vulnerability** (Marvin attack: timing side channel in RSA decryption, medium) | servo-script (WebCrypto) | No fixed version exists. Only web pages use it, through WebCrypto, with keys they make themselves in a throwaway profile that is deleted after one fetch. Juttusihti holds no RSA keys and the helper never decrypts anything of the user's. WebCrypto stays on because the news sites tested do not render without it. Re-check when Servo updates. |
| RUSTSEC-2025-0141 | bincode 1.3.3 | unmaintained | Servo internals | No runtime exposure to page input beyond Servo's own IPC. |
| RUSTSEC-2025-0075/0080/0081/0098/0100 | unic-* 0.9.0 | unmaintained | Servo (Unicode tables) | Static Unicode data. |
| RUSTSEC-2024-0436 | paste 1.0.15 | unmaintained | build-time proc macro | No runtime exposure. |

Re-run `cargo audit` before each release.

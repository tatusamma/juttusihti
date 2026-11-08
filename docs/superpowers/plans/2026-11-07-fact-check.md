# Fact Check Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A user-started **Fact check** on an article with full text: the configured AI extracts
claims, searches the web with its own search tool (plus keyless Wikipedia and an optional Google
Fact Check Tools key), judges the evidence, and the result is shown as a card above the article
and as a mark in the article list.

**Architecture:** Pure types, evidence validation, storage and estimate live in
`juttusihti-core` (`factcheck.rs`, migration 0007, a web-search price in the pricing catalog).
`juttusihti-ai` gains provider web-search support in `Request`/`Reply`, keyless/keyed evidence
sources (`sources.rs`) and the fact-check prompts and runner (`factcheck.rs`). The app gets a
`fact_check.rs` module (consent panel, progress, result card), a button in the article pane,
a list mark, and a Fact check section in AI setup.

**Tech Stack:** Rust 2024, iced 0.14, rusqlite, reqwest 0.13, serde_json, tokio, keyring.

**Spec:** `docs/superpowers/specs/2026-11-07-fact-check-design.md`

## Global Constraints

- Nothing is sent anywhere until the user presses the panel's send button (the consent).
- The fact check uses the provider from the AI defaults with its **Quality** model:
  `recommended(provider, Tier::Quality).model`, unless the defaults' explicit `model` override
  is set **and** the defaults' tier is `Quality` (then that override).
- Own per-check spending limit, default **$1.00** (`1_000_000` micros), editable in AI setup.
  A small overrun is acceptable where search counts cannot be capped exactly.
- At most **8** claims per article, most important first. At most **3** web searches per claim
  call (`max_uses`/`max_tool_calls` where the provider supports it).
- Verdicts: `Supported`, `Contradicted`, `Misleading`, `Unverified`, plus `NotChecked`
  ("Not checked (limit reached)"). Guideline checks: JSN **15, 12, 21, 16**, each
  `Ok` / `Concern` / `NotApplicable`.
- A verdict other than Unverified/NotChecked needs at least one valid non-Wikipedia source,
  otherwise it becomes `Unverified`. Valid = URL appeared in this run's gathered results, is
  http/https, not on the article's own domain, not marked as a copy of the same story.
- Gemini grounding redirect URLs are stored and shown as they are, never resolved.
- Google Fact Check results are evidence for the model; the card shows publisher and the
  rating word exactly as published. No rating mapping in code.
- Any article language. Claims quoted in the article's language; UI and reasons in English.
- Google Fact Check key lives only in the OS secret store (account `google-factcheck-api-key`).
- Wikipedia/Google requests: User-Agent `Juttusihti/<version> (https://github.com/tero-k/juttusihti)`,
  no personal details; the Google key must never appear in logs or error text.
- Unknown prices stay absent in the catalog, never 0.
- Only the latest check per article is kept; **Check again** replaces it. A cancelled run saves
  nothing (its usage is still in the ledger).
- The card's fixed note: "AI-assisted check. Verify the sources before relying on it."
- The spec's "monthly/overall limits still apply": the app has no such limit today (only
  per-run limits), so nothing extra is enforced. The spec's "consent panel says when the model
  cannot search" is done at run time instead (fallback + "Reduced check" line on the card),
  because no reliable capability table exists.
- CI gate for every task: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`.

## Review Focus

1. **Opinion piece with no checkable claims**: the card says "No checkable factual claims
   found." with the guideline checks; no claim calls are made. → test in Task 7.
2. **A web/fact-check/Wikipedia lookup fails (offline, 429, 5xx)**: the claim is still judged
   with whatever else is available; the run does not fail. → test in Task 7.
3. **Web search not available for this model/billing (e.g. a 400 naming the tool on a ChatGPT
   plan)**: the run continues without the tool for the remaining claims and the result is
   marked `web_search: false` ("Reduced check: web search was not available."). → test in Task 7.
4. **Model returns malformed or truncated JSON for one claim**: that claim becomes
   `Unverified` with reason "The check could not be completed for this claim."; others are
   unaffected. → test in Task 7.
5. **Teaser-only article or very short text**: the button is disabled with a tooltip
   "Load the full article first". → test in Task 9.

---

### Task 1: Core fact check types and evidence validation

**Files:**
- Create: `crates/core/src/factcheck.rs`
- Modify: `crates/core/src/lib.rs` (add `pub mod factcheck;`)
- Test: unit tests in `crates/core/src/factcheck.rs`

**Interfaces:**
- Produces (all `pub`, `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`, enums `#[serde(rename_all = "snake_case")]`):
  - `const MAX_CLAIMS: usize = 8; const MAX_SOURCES_PER_CLAIM: usize = 5; const FACT_CHECK_PROMPT_VERSION: u32 = 1;`
  - `enum Verdict { Supported, Contradicted, Misleading, Unverified, NotChecked }` with `fn label(self) -> &'static str` ("Supported", "Contradicted", "Misleading", "Unverified", "Not checked (limit reached)").
  - `enum SourceKind { Web, FactCheck, Wikipedia }`
  - `struct Source { url: String, title: String, kind: SourceKind, publisher: Option<String>, rating: Option<String> }`
  - `struct ClaimResult { quote: String, verdict: Verdict, reason: String, sources: Vec<Source> }`
  - `enum Guideline { Headline15, Sources12, Reply21, Advertising16 }` with `fn number(self) -> u8` and `fn label(self) -> &'static str` ("Headline backed by text", "Source criticism", "Right to reply", "Hidden advertising").
  - `enum GuidelineStatus { Ok, Concern, NotApplicable }`
  - `struct GuidelineCheck { guideline: Guideline, status: GuidelineStatus, reason: String }`
  - `struct FactCheck { article_id: ArticleId, checked_at: i64, provider: Provider, model: String, billing: Billing, prompt_version: u32, language: String, web_search: bool, claims: Vec<ClaimResult>, guidelines: Vec<GuidelineCheck>, cost: Option<Micros> }`
    - `fn flagged_count(&self) -> u32` (Contradicted + Misleading)
    - `fn summary_line(&self) -> String`, e.g. `"Fact check: 1 contradicted, 1 unverified, 2 supported"`; order contradicted, misleading, unverified, supported, not checked; zero counts omitted; no claims → `"No checkable factual claims found."`
  - `struct Gathered` (map of normalized URL → `Source`) with `fn new() -> Self`, `fn add(&mut self, s: Source)`, `fn len(&self) -> usize`.
  - `fn judge_sources(verdict: Verdict, cited: &[CitedSource], gathered: &Gathered, article_url: Option<&str>) -> (Verdict, Vec<Source>)`
  - `struct CitedSource { url: String, same_story: bool }`
  - `fn bound_text(s: &str, max_chars: usize) -> String` (trim, collapse whitespace, cut on a char boundary, append "…" when cut)

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn cited_urls_must_come_from_gathered_results() {
    // gathered: https://a.example/x (Web); cited: https://a.example/x and https://invented.example/y
    // verdict Contradicted → stays Contradicted, sources == [a.example/x]
}
#[test] fn invented_only_sources_downgrade_to_unverified() { /* cited only invented → (Unverified, []) */ }
#[test] fn own_domain_and_same_story_sources_are_dropped() {
    // article_url https://www.news.example/a/1; gathered news.example/b, sub.news.example/c, other.example/d (same_story=true), third.example/e
    // Supported → (Supported, [third.example/e])
}
#[test] fn wikipedia_alone_cannot_decide() {
    // only https://fi.wikipedia.org/wiki/X cited (kind Wikipedia), verdict Supported → Unverified, sources keep the Wikipedia entry
}
#[test] fn fact_check_source_counts_as_evidence() { /* kind FactCheck alone, Contradicted stays Contradicted */ }
#[test] fn url_normalization_ignores_fragment_case_and_trailing_slash() {
    // gathered "https://A.example/x/#frag" matches cited "https://a.example/x"
}
#[test] fn non_http_urls_are_rejected() { /* "javascript:alert(1)" in gathered and cited → dropped */ }
#[test] fn at_most_five_sources_are_kept() {}
#[test] fn summary_line_counts_verdicts() {
    // [Contradicted, Unverified, Supported, Supported] → "Fact check: 1 contradicted, 1 unverified, 2 supported"
    // [] → "No checkable factual claims found."
}
#[test] fn bound_text_cuts_on_char_boundary() { assert_eq!(bound_text("ääää  ä", 3), "äää…"); }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p juttusihti-core factcheck`
Expected: FAIL (module does not exist).

- [ ] **Step 3: Implement the types and `judge_sources` in `crates/core/src/factcheck.rs`**

Normalization: parse with `url::Url`; lowercase host, drop fragment, drop one trailing `/` of the
path. Own domain: article host with a leading `www.` removed; a source is own-domain when its
host equals it or ends with `.` + it. `NotChecked` and `Unverified` pass through unchanged
(sources still filtered).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p juttusihti-core factcheck` — Expected: PASS.

- [ ] **Step 5: Commit** — `git commit -m "Add fact check types and evidence validation"`

---

### Task 2: Storage, list flags and the per-check limit

**Files:**
- Create: `crates/core/migrations/0007_factcheck.sql`
- Modify: `crates/core/src/db.rs` (append to `MIGRATIONS`), `crates/core/src/factcheck.rs` (Store impl)
- Test: `crates/core/tests/factcheck.rs`

**Interfaces:**
- Consumes: `FactCheck` (Task 1).
- Produces on `Store`:
  - `fn save_fact_check(&self, fc: &FactCheck) -> Result<()>` (replaces any earlier row)
  - `fn fact_check(&self, article: ArticleId) -> Result<Option<FactCheck>>` (a row whose JSON no longer parses returns `None`)
  - `fn fact_check_flags(&self, articles: &[ArticleId]) -> Result<HashSet<ArticleId>>` (rows with `flagged_count > 0`)
  - `fn fact_check_limit(&self) -> Result<Micros>` (setting `fact_check_max_cost`, default `DEFAULT_FACT_CHECK_LIMIT: Micros = 1_000_000`)
  - `fn set_fact_check_limit(&self, m: Micros) -> Result<()>`

Table:

```sql
CREATE TABLE article_fact_checks (
    article_id     INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE,
    checked_at     INTEGER NOT NULL,
    provider       TEXT NOT NULL,
    model          TEXT NOT NULL,
    prompt_version INTEGER NOT NULL,
    flagged_count  INTEGER NOT NULL,
    result_json    TEXT NOT NULL
);
```

- [ ] **Step 1: Write failing tests** in `crates/core/tests/factcheck.rs` (seed a feed and two articles like `tests/fulltext.rs`):
  - `saves_and_reads_back_a_check` — round-trip equals the saved value.
  - `check_again_replaces_the_row` — second save with different claims; `fact_check` returns the second; `SELECT count(*)` is 1.
  - `flags_only_articles_with_contradicted_or_misleading` — article A has a Contradicted claim, B only Unverified → flags `{A}`.
  - `deleting_the_article_removes_its_check` — delete article row; table empty.
  - `limit_defaults_to_one_dollar_and_can_be_changed` — `1_000_000`, then `set_fact_check_limit(2_500_000)` reads back.
- [ ] **Step 2: Run** `cargo test -p juttusihti-core --test factcheck` — Expected: FAIL.
- [ ] **Step 3: Implement** the migration (appended as the 7th entry; `SCHEMA_VERSION` follows automatically) and the Store methods.
- [ ] **Step 4: Run** `cargo test -p juttusihti-core` — Expected: PASS (existing migration tests included).
- [ ] **Step 5: Commit** — `git commit -m "Store fact checks and the per-check limit"`

---

### Task 3: Web search prices and the fact check estimate

**Files:**
- Modify: `crates/core/src/pricing.rs`, `crates/core/pricing/catalog.json`, `crates/core/src/factcheck.rs`
- Test: unit tests in `pricing.rs` and `factcheck.rs`

**Interfaces:**
- Produces:
  - `Price.web_search_per_1k: Option<Micros>` (raw catalog field `web_search_per_1k`, USD per 1000 searches; absent = unknown). `Price::chatgpt_plan` sets `Some(0)`.
  - `impl Price { fn search_cost(&self, searches: u32) -> Option<Micros> }` (`searches == 0` → `Some(0)`; rounded up like `charge`).
  - In `factcheck.rs`: `struct FactCheckEstimate { expected: Option<Micros>, conservative: Option<Micros>, searches_expected: u32, searches_conservative: u32, tokens_conservative: u64 }` and
    `fn estimate(article_text: &str, price: Option<&Price>) -> FactCheckEstimate`.
  - Constants: `EXTRACT_MAX_OUTPUT: u32 = 4000`, `CLAIM_MAX_OUTPUT: u32 = 1500`, `SEARCHES_PER_CLAIM: u32 = 3`, `SEARCH_TOKENS_EXPECTED: u64 = 4000`, `SEARCH_TOKENS_CONSERVATIVE: u64 = 12000`, `CLAIM_PROMPT_TOKENS: u64 = 2500`.

Estimate model: extraction = article tokens (`estimate::approx_tokens`) + 1000 prompt, output `EXTRACT_MAX_OUTPUT` (conservative) or half (expected); claims = `MAX_CLAIMS`; expected 1 search per claim with `SEARCH_TOKENS_EXPECTED`, conservative `SEARCHES_PER_CLAIM` per claim with `SEARCH_TOKENS_CONSERVATIVE` each; claim output `CLAIM_MAX_OUTPUT` (conservative) or a third (expected). Costs are `None` when the price or its search price is unknown.

- [ ] **Step 1: Write failing tests**
  - `search_cost_rounds_up_and_zero_is_free` — price 10_000_000 per 1k (=$10): `search_cost(3) == Some(30_000)`, `search_cost(0) == Some(0)`, unknown → `None`.
  - `plan_price_has_free_searches` — `Price::chatgpt_plan("m").search_cost(5) == Some(0)`.
  - `catalog_web_search_prices_are_positive_or_absent` — every `Some` is `> 0`.
  - `estimate_conservative_exceeds_expected` and `estimate_is_unknown_without_search_price`.
- [ ] **Step 2: Run** `cargo test -p juttusihti-core` — Expected: FAIL.
- [ ] **Step 3: Implement**, and add `web_search_per_1k` to the catalog entries of the six recommended models **only after verifying each on the provider's official pricing page** (update `source`/`verified_on`). Starting points to verify: Anthropic web search $10 per 1000; OpenAI `web_search` tool $10 per 1000 calls (search content tokens are billed as input and already show in usage); Gemini 3.x Google Search grounding $14 per 1000 search queries. Ignore free daily allowances. Leave the field out where it cannot be verified.
- [ ] **Step 4: Run** `cargo test -p juttusihti-core` — Expected: PASS.
- [ ] **Step 5: Commit** — `git commit -m "Price web searches and estimate fact checks"`

---

### Task 4: Web search in provider requests and replies

**Files:**
- Modify: `crates/ai/src/lib.rs`, `crates/ai/src/providers.rs`, `crates/ai/src/prompts.rs` and `crates/ai/src/runner.rs` (existing `Request { .. }` literals get `web_search: None`), `crates/ai/src/runner.rs` (`Control::is_cancelled`)
- Test: unit tests in `providers.rs`

**Interfaces:**
- Produces in `juttusihti_ai`:
  - `pub struct WebSearch { pub max_uses: u32 }` and `Request.web_search: Option<WebSearch>`
  - `#[derive(Debug, Clone, Default, PartialEq)] pub struct WebEvidence { pub searches: u32, pub sources: Vec<WebSource> }`, `pub struct WebSource { pub url: String, pub title: String }`
  - `Reply.web: WebEvidence` (empty for requests without search)
  - `impl Control { pub fn is_cancelled(&self) -> bool }`
  - `pub fn is_tool_unsupported(e: &AiError) -> bool` — `Status { status: 400 | 404, message }` whose lowercased message contains `"tool"`, `"web_search"` or `"google_search"`, or `AiError::Unsupported(_)`.

Request shapes (verify the current names in each provider's docs via context7 before coding; keep the shape in one function per provider):
- Anthropic: `"tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": n}]`. If the API rejects `output_config.format` together with tools, drop the format for search requests and rely on the prompt plus `json_or_text`.
- OpenAI (API key and ChatGPT plan): `"tools": [{"type": "web_search"}]`, `"max_tool_calls": n`, `"include": ["web_search_call.action.sources"]`.
- Gemini: `"tools": [{"google_search": {}}]` (no cap available).

Parsing:
- Anthropic: `usage.server_tool_use.web_search_requests` → `searches`; sources from `web_search_tool_result` blocks (`content[].url/title`) and text-block `citations[].url/title`.
- OpenAI: count `output[]` items of type `web_search_call`; sources from their `action.sources[].url` and from `output_text` `annotations[]` of type `url_citation`.
- Gemini: `searches` = `candidates[0].groundingMetadata.webSearchQueries.len()`; sources from `groundingChunks[].web.uri/title`.
- Dedupe sources by URL, keep at most 50, drop non-http(s).

- [ ] **Step 1: Write failing tests** (`builds_web_search_requests`, `parses_anthropic_web_evidence`, `parses_openai_web_evidence`, `parses_gemini_grounding`, `requests_without_search_have_no_tools`, `tool_errors_are_recognized`) with hand-written JSON replies in the shapes above; assert `max_uses == 3` / `max_tool_calls == 3`, the counted searches, and the URL lists.
- [ ] **Step 2: Run** `cargo test -p juttusihti-ai providers` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p juttusihti-ai` — Expected: PASS (existing runner tests unchanged).
- [ ] **Step 5: Commit** — `git commit -m "Support provider web search in AI requests"`

---

### Task 5: Evidence sources: Google Fact Check and Wikipedia

**Files:**
- Create: `crates/ai/src/sources.rs` (`pub mod sources;` in `lib.rs`)
- Test: unit tests (parsing) in `sources.rs`; HTTP tests in `crates/ai/tests/sources.rs` against a local `TcpListener` server

**Interfaces:**
- Produces:
  - `pub struct PublishedCheck { pub claim_text: String, pub claimant: Option<String>, pub publisher: String, pub url: String, pub title: String, pub rating: String, pub review_date: Option<String> }`
  - `pub struct WikiPage { pub title: String, pub url: String, pub extract: String }`
  - `pub struct Sources` with
    - `pub fn new(google_key: Option<String>) -> Result<Self, String>` (bases `https://factchecktools.googleapis.com` and `https://{lang}.wikipedia.org`)
    - `pub fn with_bases(google_key: Option<String>, google_base: &str, wikipedia_base: &str) -> Result<Self, String>` (`wikipedia_base` contains `{lang}`; for tests)
    - `pub fn has_google(&self) -> bool`
    - `pub async fn fact_checks(&self, query: &str, lang: &str) -> Result<Vec<PublishedCheck>, String>` — `GET {google}/v1alpha1/claims:search?query=…&languageCode={lang}&pageSize=5&key=…`; if empty, once more without `languageCode`. Without a key: `Ok(vec![])`, no request.
    - `pub async fn wikipedia(&self, query: &str, lang: &str) -> Result<Vec<WikiPage>, String>` — `GET {base}/w/rest.php/v1/search/page?q=…&limit=2`; `url` = `{base}/wiki/{key}`; `extract` = excerpt with HTML tags removed, bounded to 500 chars.
  - `pub fn valid_lang(lang: &str) -> &str` — `^[a-z]{2,3}$` else `"en"`.
- reqwest client: 15 s timeout, no redirects, 1 MiB body cap, User-Agent from Global Constraints. Errors use `e.without_url()` so the key never appears.

- [ ] **Step 1: Write failing tests**: `parses_claim_search_reply` (one claim, two reviews → two `PublishedCheck`s with publisher/rating as given, e.g. rating `"Väärä"`), `parses_wikipedia_search_and_strips_tags`, `invalid_lang_falls_back_to_english`, and in `tests/sources.rs`: `retries_without_language_when_nothing_found` (server sees two requests, second without `languageCode`), `no_key_sends_nothing`, `errors_do_not_contain_the_key` (server answers 403; error string lacks the key), `sends_generic_user_agent`.
- [ ] **Step 2: Run** `cargo test -p juttusihti-ai sources` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p juttusihti-ai` — Expected: PASS.
- [ ] **Step 5: Commit** — `git commit -m "Look up published fact checks and Wikipedia"`

---

### Task 6: Fact check prompts and output parsing

**Files:**
- Create: `crates/ai/src/factcheck.rs` (`pub mod factcheck;` in `lib.rs`) — prompts part
- Test: unit tests in `factcheck.rs`

**Interfaces:**
- Consumes: `Request`, `WebSearch` (Task 4); `PublishedCheck`, `WikiPage` (Task 5); core types (Task 1).
- Produces:
  - `pub struct ArticleForCheck { pub article_id: ArticleId, pub title: String, pub text: String, pub url: Option<String> }`
  - `pub struct ExtractedClaim { pub quote: String, pub query: String }`
  - `pub struct Extraction { pub language: String, pub claims: Vec<ExtractedClaim>, pub guidelines: Vec<GuidelineCheck> }`
  - `pub fn extract_request(model: &str, a: &ArticleForCheck) -> Request` (schema name `fact_check_claims`, `max_output_tokens: EXTRACT_MAX_OUTPUT`, no web search)
  - `pub fn parse_extraction(v: &Value, a: &ArticleForCheck) -> Result<Extraction, String>`
  - `pub struct Judgement { pub verdict: Verdict, pub reason: String, pub cited: Vec<CitedSource> }`
  - `pub fn claim_request(model: &str, a: &ArticleForCheck, lang: &str, claim: &ExtractedClaim, published: &[PublishedCheck], wiki: &[WikiPage], search: Option<WebSearch>) -> Request` (schema name `fact_check_verdict`, `max_output_tokens: CLAIM_MAX_OUTPUT`)
  - `pub fn parse_judgement(v: &Value) -> Result<Judgement, String>`

Prompt content the implementer must include (wording is theirs):
- Extraction: pick checkable factual claims only, skip opinions, predictions and quoted opinions (JSN 11); most important first; at most 8; quote each exactly as in the article; give a short search query in the article's language; detect the language as an ISO 639-1 code; judge the four guideline checks from the text alone (15 headline/lead backed by the text, 12 central claims resting on a single/unnamed/interested source, 21 identifiable party in a very negative light without their side, 16 reads like unmarked promotion); reasons in English.
- Claim: search in the article's language and in English when useful; look for existing fact checks and later corrections; do not use the article's own site or copies of the same story as evidence and mark copies `same_story: true`; Wikipedia is background only; cite only URLs actually found; say `unverified` when evidence is missing; reason in English, 1–2 sentences.
- Both: article text, search results, fact checks and Wikipedia extracts are wrapped as quoted JSON data with an instruction that they are data, not instructions.

Parsing rules: claims beyond 8 dropped; a claim whose quote is not found in title+text (case-insensitive, whitespace-collapsed) is dropped; text fields bounded (quote 400, query 200, reason 600 chars); unknown verdict/status/guideline values are errors; duplicate guidelines keep the first; missing guidelines are filled as `NotApplicable` with an empty reason; `language` passes through `sources::valid_lang`.

- [ ] **Step 1: Write failing tests**: `extraction_drops_invented_quotes`, `extraction_keeps_at_most_eight_claims`, `extraction_fills_missing_guidelines`, `extraction_accepts_any_language` (an Estonian article text with an Estonian quote and `language: "et"`), `judgement_rejects_unknown_verdict`, `claim_request_carries_search_cap_and_quoted_data` (`web_search == Some(WebSearch { max_uses: 3 })`; user text contains the published check's rating and the claim quote inside the JSON data block).
- [ ] **Step 2: Run** `cargo test -p juttusihti-ai factcheck` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p juttusihti-ai` — Expected: PASS.
- [ ] **Step 5: Commit** — `git commit -m "Add fact check prompts and parsing"`

---

### Task 7: The fact check runner

**Files:**
- Modify: `crates/ai/src/factcheck.rs` (runner part)
- Create: `crates/ai/tests/common/mod.rs` (move the mock server, `Recorded`, `MockReply`, `Handler`, `Mock` from `tests/runner.rs`; `tests/runner.rs` uses `mod common;`), `crates/ai/tests/factcheck.rs`

**Interfaces:**
- Consumes: Tasks 1–6; `Client`, `Control`, `Guard`, `Store::record_usage`, `Store::save_fact_check`.
- Produces:
  - `pub enum FactCheckProgress { Extracting, Checking { done: usize, total: usize } }`
  - `pub struct FactCheckJob { pub article: ArticleForCheck, pub model: String, pub limit: Micros }`
  - `pub enum FactCheckError { Cancelled, Ai(String), Store(String) }` (`thiserror`, user-readable messages)
  - `pub async fn run_fact_check(job: FactCheckJob, client: &Client, sources: &Sources, catalog: &Catalog, db: Arc<Mutex<Store>>, control: &Control, progress: mpsc::UnboundedSender<FactCheckProgress>) -> Result<FactCheck, FactCheckError>`

Behaviour:
- Price: `catalog.price_for(provider, model, billing, today)`; `Guard::new(Limits { max_cost: Some(limit), max_tokens: None, max_calls: None, allow_unknown_cost: false }, Totals::default())`.
- Extraction: reserve worst case, send, settle, `record_usage(None, …, "fact_check_extract", …)`; one retry on `Truncated`/`NotJson`/parse error, then `FactCheckError::Ai("Could not read the article's claims.")`. A refusal of the reservation before extraction → `Ai("The fact check limit is too low for this article.")`.
- Zero claims → save and return a `FactCheck` with no claims and the guidelines.
- Claims run with at most 3 in flight, in importance order. Before each: if cancelled → `Cancelled` (save nothing); reserve worst tokens cost + `search_cost(SEARCHES_PER_CLAIM)`; on `Refusal` this and all not-yet-started claims become `NotChecked` with reason "Not checked (limit reached)".
- Per claim: `sources.fact_checks(query, lang)` and `sources.wikipedia(query, lang)` (errors → empty, logged at debug); send `claim_request(… Some(WebSearch { max_uses: SEARCHES_PER_CLAIM }))` while web search is on. If `is_tool_unsupported(&e)`, turn web search off for this and all later claims (shared flag), resend without it, and set `FactCheck.web_search = false`.
- Cost of a claim call = `price.cost(usage) + price.search_cost(reply.web.searches)`; settle with it (may exceed the reservation — accepted), `record_usage(None, …, "fact_check_claim", …)`.
- `Gathered` = reply web sources (`Web`) + published checks (`FactCheck`, with publisher and rating) + Wikipedia pages (`Wikipedia`); `judge_sources(…, article.url)`.
- Output failure after one retry → `Unverified`, reason "The check could not be completed for this claim."
- Final `FactCheck.cost` = sum of settled costs (`None` if any unknown); save with `save_fact_check`; return it.

- [ ] **Step 1: Move the mock** to `tests/common/mod.rs`; run `cargo test -p juttusihti-ai --test runner` — Expected: PASS unchanged.
- [ ] **Step 2: Write failing tests** in `tests/factcheck.rs` (mock answers OpenAI Responses JSON; a second mock serves Google/Wikipedia via `Sources::with_bases`):
  - `checks_claims_and_saves_the_result` — 2 claims; claim 1 cites a returned web source → Contradicted kept; claim 2 cites an invented URL → Unverified; saved row equals the returned value; ledger has 3 rows with operations `fact_check_extract`, `fact_check_claim` ×2.
  - `opinion_piece_makes_no_claim_calls` — extraction returns no claims → 1 provider request, summary "No checkable factual claims found."
  - `lookup_failures_do_not_fail_the_run` — Google and Wikipedia mock return 500; verdict still from web sources.
  - `unsupported_web_search_falls_back` — first claim call returns 400 "Tool 'web_search' is not supported"; the resent request has no `tools`; result `web_search == false`.
  - `malformed_claim_output_becomes_unverified` — one claim reply is non-JSON twice.
  - `limit_marks_remaining_claims_not_checked` — limit fits extraction + 1 claim → later claims `NotChecked`.
  - `cancel_saves_nothing` — cancel during the first claim call (mock delay) → `Err(Cancelled)`, `fact_check(article) == None`.
  - `google_published_rating_reaches_the_card` — published check rating `"Väärä"` cited → source kind `FactCheck`, `rating == Some("Väärä")`.
- [ ] **Step 3: Run** `cargo test -p juttusihti-ai --test factcheck` — Expected: FAIL.
- [ ] **Step 4: Implement `run_fact_check`.**
- [ ] **Step 5: Run** `cargo test -p juttusihti-ai` — Expected: PASS.
- [ ] **Step 6: Commit** — `git commit -m "Run fact checks within their own limit"`

---

### Task 8: Google key and limit in AI setup

**Files:**
- Modify: `crates/app/src/secrets.rs`, `crates/app/src/providers.rs`
- Test: unit tests in `providers.rs` (follow the file's existing test style)

**Interfaces:**
- Produces in `secrets`: `pub fn google_factcheck_key() -> Result<Option<String>, String>`, `pub fn set_google_factcheck_key(key: &str) -> Result<(), String>` (same validation as `set`), `pub fn delete_google_factcheck_key() -> Result<(), String>`; account `google-factcheck-api-key`.
- AI setup gets a **Fact check** section after the provider keys:
  - heading "Fact check"
  - note "Fact checks use your provider's quality model and its web search."
  - "Limit per fact check (USD)" field, default `1.00`; saved through `Store::set_fact_check_limit`; invalid or ≤ 0 input shows "Enter an amount above 0." and is not saved.
  - "Google Fact Check Tools API key (optional)" key row with Save/Remove, hint "Free from Google Cloud. Adds published fact checks (Faktabaari, AFP and others) and makes checks more accurate.", link to `https://console.cloud.google.com/apis/library/factchecktools.googleapis.com`.
- [ ] **Step 1: Write failing tests**: `limit_input_parses_dollars` (`"1.00"` → `1_000_000`, `"0"` and `"abc"` rejected), and a `Msg` round-trip test that saving the limit updates the stored value (use an in-memory `Db` the way other app tests do).
- [ ] **Step 2: Run** `cargo test -p juttusihti providers` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p juttusihti` — Expected: PASS.
- [ ] **Step 5: Commit** — `git commit -m "Set up fact checks in AI setup"`

---

### Task 9: Fact check button, consent panel, progress and result card

**Files:**
- Create: `crates/app/src/fact_check.rs` (`mod fact_check;` in `main.rs`)
- Modify: `crates/app/src/app.rs` (`Preview.fact_check`, button, `Message::FactCheck(fact_check::Msg)`, routing), `crates/app/src/style.rs` (`pub fn success(theme) -> text::Style`)
- Test: unit tests in `fact_check.rs` and `app.rs`

**Interfaces:**
- Consumes: `run_fact_check`, `Sources`, `estimate`, `Store::fact_check`, `research::cost_text`, `ai_setup::defaults/readiness`, the endpoint selection used by `research::run_session` (factor `api_key_endpoint` + plan session into `pub(crate) async fn endpoint(provider, billing) -> Result<Endpoint, String>` in `research.rs` and use it in both places).
- Produces:
  - `pub const MIN_CHECK_CHARS: usize = 300;`
  - `pub fn can_check(has_fulltext: bool, feed_content: Option<&str>, body: &str) -> bool` — `(has_fulltext || feed_content non-empty) && body.chars().count() >= MIN_CHECK_CHARS`
  - `fn fact_check_model(d: &AiDefaults) -> String` (rule in Global Constraints)
  - `pub struct State` with `pub fn new(db: Db) -> Self`, `pub fn update(&mut self, msg: Msg) -> Cmd<Msg>`, `pub fn view_panel(&self, article: ArticleId) -> Option<Element<'_, Msg>>`, `pub fn is_running(&self) -> bool`
  - `pub fn view_card<'a>(fc: &'a FactCheck, collapsed: bool) -> Element<'a, Msg>`
  - `pub struct Plan { pub article: ArticleForCheck, pub provider: Provider, pub billing: Billing, pub model: String, pub estimate: FactCheckEstimate, pub limit: Micros, pub has_google: bool, pub ready: Result<(), String> }` (built off the UI thread when the panel opens; reads the Google key's presence, not its value)
  - `pub enum Msg { Open(ArticleId), Planned(u64, Result<Box<Plan>, String>), Send, Cancel, Close, Progress(FactCheckProgress), Finished(ArticleId, Result<Box<FactCheck>, String>), OpenLink(String), ToggleCard }`; the app reloads the preview on `Finished(Ok)`.

Behaviour:
- Button "Fact check" in the article action row after "Fetched"/"Read full article". Disabled with tooltip "Load the full article first" when `!can_check(..)`; "Check again" when a result exists; "Checking… n/m" while running.
- Panel (inline, under the action row): provider and model; line "Sends the article to {provider}{, its claims to Google Fact Check}{ and searches to Wikipedia}."; estimate "About {expected}, at most about {conservative}" via `cost_text` (or "cost unknown"); line "Web searches can run slightly over the limit." when provider is OpenAI or Gemini; limit "Limit {limit}"; buttons "Check with {provider}" (the consent; disabled when credentials are not ready or cost is unknown on an API key) and "Cancel". Nothing is sent before "Check with …".
- Card (layout B, `style::card`): header = `summary_line()` coloured by the worst verdict (danger for Contradicted, accent for Misleading/Unverified, success otherwise), then "Checked {date} · {provider} {model}" and "Check again"; "Reduced check: web search was not available." when `!web_search`; one row per claim: tag (verdict label, coloured), quote, reason, source links (title, or domain when the title is empty; FactCheck sources as "{publisher}: {rating}") opening via `open::that_detached`; guideline rows "JSN {n} {label}: OK/Concern/Not applicable" with reason for concerns; the fixed note. Collapsible header; expanded after a run.
- [ ] **Step 1: Write failing tests**: `can_check_needs_full_text` (teaser-only → false, fetched 400-char body → true, feed content 299 chars → false), `fact_check_uses_quality_model` (Fast defaults with a Fast model override still → `recommended(p, Quality).model`; Quality defaults with override → override), `consent_panel_sends_nothing_until_send` (after `Open` and `Planned`, no run is active), and in `app.rs` `preview_loads_saved_fact_check`.
- [ ] **Step 2: Run** `cargo test -p juttusihti fact_check` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test -p juttusihti` — Expected: PASS. Then launch the app (`cargo run -p juttusihti`) with a copy of the user's database (see memory: drive the app window by its HWND, never screen clicks) and check that the card renders for a stored check; take a screenshot.
- [ ] **Step 5: Commit** — `git commit -m "Fact check articles from the reading pane"`

---

### Task 10: Mark flagged articles in the list

**Files:**
- Modify: `crates/app/src/app.rs` (`Listing.flagged: HashSet<ArticleId>`, loaded with `fact_check_flags` where `headlines` is loaded; `view_row`)
- Test: `app.rs` unit test

- [ ] **Step 1: Write failing test** `listing_marks_flagged_articles`: a stored check with a Contradicted claim for article A → `listing.flagged == {A}`; after **Check again** saves a clean result and the list reloads, the set is empty.
- [ ] **Step 2: Run** `cargo test -p juttusihti listing_marks` — Expected: FAIL.
- [ ] **Step 3: Implement**: in `view_row`, a small `danger`-coloured "!" (SIZE_SMALL) before the "Source · time" text with tooltip "Fact check found contradicted or misleading claims". Reload the list after a fact check finishes.
- [ ] **Step 4: Run** `cargo test -p juttusihti` — Expected: PASS.
- [ ] **Step 5: Commit** — `git commit -m "Mark fact-check findings in the article list"`

---

### Task 11: Help, README and changelog

**Files:**
- Modify: `crates/app/src/help.rs`, `README.md` (Features list and Using section), `CHANGELOG.md` (Unreleased)

- [ ] **Step 1:** Help and README: what Fact check does, that it starts only on the button, what is sent where (AI provider; Google Fact Check with a key; Wikipedia), the per-check limit and possible small overrun, the verdicts and the JSN guideline checks with a link to https://jsn.fi/journalistin-ohjeet/, and that results are AI-assisted. Keep the README's privacy sentence true ("Nothing is sent anywhere you did not ask for").
- [ ] **Step 2:** Run the CI gate from Global Constraints — Expected: all pass.
- [ ] **Step 3: Commit** — `git commit -m "Document fact checks"`

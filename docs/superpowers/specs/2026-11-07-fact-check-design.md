# Fact check: check a suspicious article's claims on demand

Date: 2026-11-07. Applies to the workspace at 0.4.0 (`crates/ai`, `crates/core`, `crates/app`).

## Why

When a news article feels off, the user wants to check whether its claims are actually true,
without leaving Juttusihti. One button: the configured AI pulls out the factual claims, searches
for evidence and existing fact checks, weighs the evidence and shows a verdict per claim, with
sources. Problems are flagged clearly.

The check also follows the Finnish journalists' guidelines (*Journalistin ohjeet*, JSN, in force
1.10.2024, https://jsn.fi/journalistin-ohjeet/), which the user named as the reference for how
news should be fact-checked.

## Goals

- A **Fact check** button on an article whose full text has been loaded. Nothing runs until the
  user presses it.
- Works **without any extra API keys**: uses the AI provider and credentials the user already
  configured (OpenAI API key, ChatGPT sign-in, Anthropic or Gemini), plus Wikipedia (keyless).
- An **optional Google Fact Check Tools API key** in Settings makes it more accurate by adding
  published fact checks (Faktabaari, AFP, etc.).
- Each checked claim gets a verdict, a short reason and source links.
- A few journalism-guideline checks next to the claims (below).
- The result is shown as a **card above the article text** (user chose layout B in the mockup);
  the article text itself is not changed or highlighted.
- Results are kept, so reopening the article shows the card again without paying twice.

## Non-goals

- No automatic or background checking of articles. Only the button starts a check.
- No highlighting inside the article text (layouts A/C were not chosen).
- No checking of teaser-only articles (100 characters is nothing to check).
- No sign-up-required search services by default (Brave Search, Bing, SerpAPI…).
- No scraping of search engines with a disguised HTTP client (same rule as the full-article
  design: the app does not pretend to be a browser).

## Anti-goals (would be a failure even if it works)

- Presenting the AI's guesses as fact. Every verdict must cite where it came from; a claim
  without evidence is **unverified**, never "probably true" or "false".
- Sending anything anywhere before the user consents to the run.
- Spending money without the usual estimate and spending limits.
- Calling opinions false. Opinions are left out of the claim list (JSN 11).

## The flow

1. User opens an article, has loaded the full text (**Read full article**), finds it suspicious
   and presses **Fact check** in the article's button row (next to Save / Open in browser /
   Fetched / Connections). *(My call: the button is shown only when full text exists; for a
   feed that already gives full text in `content_text`, that counts too.)*
2. A consent panel opens, like the Quick AI panel (`crates/app/src/quick.rs`): provider from
   the AI defaults with its Quality model (see Model), a local estimate of tokens, searches and cost, and a send button
   that names the provider and where data goes (the AI provider; Google Fact Check if a key is
   set; Wikipedia). Pressing send is the consent. There is no separate per-claim approval step
   (user confirmed).
3. The run, in stages:
   1. **Claim extraction.** The AI reads the headline, lead and full text and returns the
      checkable factual claims (at most ~8, *my call*, ordered by importance), each quoted as it
      appears in the article, plus a search-friendly restatement. Opinions, predictions and
      quotes of opinion are skipped (JSN 11). The headline/lead are included so their backing
      can be judged (JSN 15).
   2. **Existing fact checks.** If a Google Fact Check Tools key is set, query
      `claims:search` per claim (try the article's language, e.g. `fi`, then without a
      language filter). Without the key, the AI's web search is asked to look for existing
      fact checks as part of step 3.
   3. **Live research.** The AI provider's own web search tool (Anthropic web search server
      tool, OpenAI Responses `web_search` tool, Gemini Google Search grounding) searches for
      evidence for each claim. Wikipedia's keyless search/summary API is queried for
      background on named people, places and organisations.
   4. **Evidence evaluation.** The AI gets the claims, the found sources (with URLs and
      snippets) and the fact checks, and returns per claim a verdict, a one/two-sentence
      reason and the sources it relied on. It also returns the guideline checks.
4. The result card appears above the article text.

## Verdicts

| Verdict | Meaning | Colour (existing theme tokens) |
| --- | --- | --- |
| Contradicted | Reliable sources say otherwise | `danger` |
| Misleading | Partly true, missing context, or exaggerated | `accent` (amber) |
| Unverified | No independent source found either way | `accent` (amber) |
| Supported | Independent sources confirm it | `success` |

A verdict other than *Unverified* must cite at least one source URL that came from the search or
fact-check results (validated, see below). An existing published fact check shown with its
publisher and rating.

## Guideline checks (JSN)

Shown under the claims, each with its guideline number so the user can look it up:

- **15 Headline backed by text**: do the headline and lead say more than the article supports?
- **12 Source criticism**: do central claims rest on a single, unnamed or interested source?
- **21 Right to reply**: is an identifiable person or party shown in a very negative light
  without their side in the same article?
- **16 Hidden advertising**: does the piece read like promotion without being marked as such?

Each is *OK*, *Concern* (with a one-sentence reason) or *Not applicable*. These are judgements
from the article text alone; they need no web search. *(My call: corrections (JSN 35–36) are
covered by the web search step only in passing: if a later correction of the same story is
found, it is mentioned in the relevant claim's reason. No separate check.)*

## The result card (layout B)

- Top line: "Fact check: 1 contradicted, 1 unverified, 2 supported", coloured by the worst
  verdict, plus the date checked, provider/model, and **Check again**.
- One row per claim: verdict tag, the claim quoted from the article, the reason, source links
  (open in the system browser, like **Open in browser**).
- The guideline checks as rows with a grey **JSN nn** tag.
- A short fixed note: "AI-assisted check. Verify the sources before relying on it."
- Collapsible, *my call*: expanded right after a run, remembers collapsed state per session only.

## Alert in the article list

*(My call, from the user's original "alert/highlight" request; they did not answer the direct
question.)* An article whose saved fact check has at least one **Contradicted** or
**Misleading** claim gets a small `danger`-coloured mark in the article list row, next to
the source/time line. No mark for unverified-only or clean results.

## Storage

- New migration `0007_factcheck.sql`. One row per article holding the latest check: article id,
  created at, provider, model, prompt version, the validated result as JSON (claims, verdicts,
  reasons, sources, guideline checks), and a count of contradicted/misleading claims for the list
  mark. **Check again** replaces it. *(My call: only the latest check is kept.)*
- Usage of every call goes into the existing `usage_ledger`, and the run counts against the
  existing spending limits through the budget `Guard`.
- Search results themselves are not stored beyond the sources cited in the result.

## Keys and Settings

- Settings gets an optional **Google Fact Check Tools API key** field, stored in the OS secret
  store via `crates/app/src/secrets.rs` (new account name, e.g. `google-factcheck-api-key`),
  never in the database. Same validation and remove button as the AI keys. A one-line hint says
  it is free from Google Cloud and makes fact checks more accurate.
- No key field for any search engine.

## Model

Fact check always uses the provider's **Quality** tier model (not the default Fast tier), for
both claim extraction and the search-and-judge calls (user decided: higher-quality models). The
provider and billing (API key or ChatGPT sign-in) still come from the AI defaults. If the
Quality model on that provider cannot use web search, the consent panel says so and offers the
reduced check (Wikipedia + Google Fact Check if keyed).

## Spending limit

- Fact check has **its own per-check limit**, separate from the per-run AI limit, editable in
  Settings next to the other AI limits. *(My call: default $1.00.)*
- **A small overrun is acceptable** (user decided). Where the provider allows it, the number of
  searches is capped per call (Anthropic `max_uses`; OpenAI's tool-call limit where available).
  Where it cannot be capped exactly, the budget books an approximate worst case, and the run may
  end slightly above it. The consent panel says so in one line. Searches are sent one claim
  per call, and the run stops starting new claims once the limit is reached; the claims not
  checked are shown as *Not checked (limit reached)*.
- The monthly/overall spending limits still apply as for every AI run.

## Cost and estimate

- The estimate shows: claim extraction tokens, number of web searches (expected: one per claim,
  conservative: two or three per claim), search fees and evaluation tokens, using the same
  expected/conservative scenarios as other runs.
- Web search fees are a per-search price, not a token price. The pricing catalog
  (`crates/core/pricing/catalog.json`) needs a per-provider web search price with source and
  verified date, following the catalog's rule: unknown prices stay absent, never 0, and the
  estimate then says the cost is unknown.
- On a ChatGPT plan the run is charged to the plan, as with other runs.

## Evidence rules *(my calls, after the builder check)*

- **What counts as evidence:** sources from the article's own domain, and copies of the same
  story (same wire text, e.g. STT, or near-identical wording) are thrown out in code before a
  verdict can rest on them. Wikipedia is **context only**: it can appear in a reason but cannot
  on its own make a claim Supported or Contradicted. A verdict resting only on excluded or
  context-only sources is downgraded to *Unverified*.
- **One call searches and judges:** the AI's web search and the evaluation for a claim happen
  in the same provider call (built-in search tools do not reliably hand back full result text).
  Validation is then "every cited URL appeared in that call's tool results or citations".
  Gemini's grounding links are Google redirect URLs: they are stored and shown as they are,
  labelled with the source's domain/title from the grounding data, and are **not** resolved by
  the app (that would be an extra unconsented request). They may stop working over time; the
  domain label stays.
- **Published fact checks are evidence, not the verdict:** a matching Google Fact Check result
  goes to the model as strong evidence, and the card always shows it with its publisher and
  its rating word as published ("Väärä", "False"…). No fixed rating mapping in code.
- **Search pricing:** the catalog gets a web search price per model (or model family), with
  source and verified date. Free daily allowances are ignored, so the estimate stays on the
  safe side.

## Safety and validation

Model output and web content are untrusted (as in `crates/ai/src/lib.rs`):

- Every AI step uses a JSON schema, and the result is validated before saving: verdict values
  from the fixed set, text lengths bounded, source URLs must be http/https **and** must appear
  in the search/fact-check results gathered in this run (a model cannot invent a source).
  A verdict whose sources fail validation is downgraded to *Unverified*.
- Web page text and snippets are passed to the AI as quoted data, with an instruction that they
  are not instructions.
- Nothing from the result is executed or fetched automatically; links open only on click.
- Wikipedia requests use a generic app User-Agent (`Juttusihti/<version>` with the repository
  URL), no personal contact details.

## What's left to the builder

- Exact request shapes for each provider's web search tool and how to combine it with
  JSON-schema output. If a provider does not allow both in one call, split into a search call
  (free text with citations) and an evaluation call (JSON schema). Check that web search works
  with ChatGPT sign-in; if it does not, say so in the consent panel and fall back to Wikipedia
  plus Google Fact Check (if keyed), with more claims ending up *Unverified*.
- How many claims are searched per request (one per claim, or batched).
- Prompt wording, prompt version constant, and the claim limit.
- UI language follows the rest of the app (English); claims are quoted in the article's own
  language; reasons are written in English. *(My call.)*
- Tests in the style of the existing ones: fake provider for the runner, validation tests for
  invented sources, and a store test for the migration.

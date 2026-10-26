# Benchmarks

Synthetic data from `crates/bench`: 50 feeds over two years; Finnish and English text drawn from a
~60-word vocabulary; 20% of articles with archived full text; 2% without a publication date. 10% of
articles are labelled, 5% have entities and 2% have story links.

The vocabulary is deliberately tiny, so common words match up to half of all articles. That is far
more than real news, so text-search figures are a worst case.

Machine: Windows 11 x64, release build (`cargo run --release -p juttusihti-bench -- <n> <dir>`).
Query rows are the median of 7 runs. Measured 2026-10-26.

| Measurement | 10k | 100k | 1M | 1M before tuning |
|---|---|---|---|---|
| Ingest (batches of 2,000) | 643.5 ms (15541 articles/s) | 9.08 s (11011 articles/s) | 135.96 s (7355 articles/s) | 116.74 s (8566 articles/s) |
| Research data (labels/entities/links) | 26.2 ms | 263.1 ms | 2.71 s | 2.73 s |
| Database size | 22.2 MB (2332 bytes/article) | 216.6 MB (2272 bytes/article) | 2168.6 MB (2274 bytes/article) | 2101.6 MB (2204 bytes/article) |
| Open existing database | 2.1 ms | 2.0 ms | 2.0 ms | 2.2 ms |
| List: All, first page | 0.3 ms | 0.3 ms | 0.2 ms | 74.8 ms |
| List: Unread, page 100 | 14.2 ms | 17.6 ms | 16.6 ms | 4.97 s |
| List: one feed, first page | 0.1 ms | 0.1 ms | 0.3 ms | 0.3 ms |
| Search: common word | 15.1 ms | 32.6 ms | 39.9 ms | 1.44 s |
| Search: prefix | 19.9 ms | 90.2 ms | 861.6 ms | 1.50 s |
| Search: phrase | 8.5 ms | 45.5 ms | 115.6 ms | 412.8 ms |
| Search: boolean | 18.5 ms | 44.0 ms | 62.7 ms | 1.31 s |
| Search: NEAR | 15.2 ms | 54.6 ms | 65.9 ms | 1.17 s |
| Search: word + feed + 30-day range | 12.9 ms | 95.5 ms | 917.9 ms | 1.03 s |
| Filter: label (confirmed) | 1.2 ms | 5.9 ms | 45.9 ms | 227.2 ms |
| Filter: entity | 0.7 ms | 5.1 ms | 33.4 ms | 236.8 ms |
| Filter: 7-day range | 0.1 ms | 0.1 ms | 0.3 ms | 0.2 ms |
| Article relationships | 0.0 ms | 0.0 ms | 0.0 ms | 0.0 ms |
| Analysis scope: 30 days, max 1,000 (dedup, cache, candidates) | 4.2 ms | 30.0 ms | 32.0 ms | 32.9 ms |
| matched / distinct / candidate pairs | 379 / 379 / 895 | 3963 / 1000 / 2470 | 40295 / 1000 / 2470 | 40295 / 1000 / 2470 |
| Estimate for that scope | 0.8 ms | 2.2 ms | 2.4 ms | 2.4 ms |
| Timeline: 30 days (≤2,000 articles) | 6.8 ms | 41.9 ms | 46.5 ms | 50.8 ms |
| Graph: 2 hops around a linked article | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms |
| Graph layout (2 nodes) | 0.0 ms | 0.0 ms | 0.0 ms | 0.0 ms |
| Graph layout (150 nodes, cap) | 5.7 ms | 5.7 ms | 5.7 ms | 5.7 ms |
| DuckDB snapshot (copy from SQLite) | 43.1 ms | 180.5 ms | 1.58 s | 1.65 s |
| Report: volume by source, 1 year/week | 2.3 ms | 4.1 ms | 9.6 ms | 9.9 ms |
| Report: topic evolution, 1 year/month | 3.8 ms | 4.7 ms | 9.6 ms | 10.2 ms |
| Report: entity co-occurrence | 4.3 ms | 5.5 ms | 11.9 ms | 11.9 ms |
| Report: labels by source | 4.5 ms | 5.5 ms | 11.2 ms | 10.9 ms |

## Tuning applied (migration 0004 and two-step search)

- **List indexes:** indexes now follow the list sort order, `(effective_at DESC, id DESC)`, with
  partial versions for Unread and Saved. *Unread, page 100* went from 4.97 s to 17 ms at 1M.
- **Capped counts:** counts stop at 10,000 and the UI shows "10,000+".
- **Two-step text search:** the page is chosen from ids and ranks only; snippets are computed for
  that page alone. Above 10,000 matches, ranking covers the 10,000 most recently stored matches,
  and the UI says so.
- **Ingest index:** an exact `(feed_id, dedup_key)` index for the duplicate check. Without it,
  ingest dropped to ~1,000 articles/s, and a query-plan test now guards this.

Trade-off: the extra indexes make bulk ingest at 1M about 14% slower (8,566 → 7,355 articles/s) and
the database about 3% larger. Normal feed refreshes are tiny batches, so this is not noticeable.

## Known slow cases at 1M (worst-case synthetic vocabulary)

- **Prefix search on a term present in ~50% of articles (~0.9 s).** FTS5 merges every matching
  document list for a prefix query.
- **A very common word combined with a highly selective feed/date filter (~0.9 s).** Few results
  match, so counting cannot stop early.

Both depend on how common the term is; realistic vocabularies match far fewer documents.

## Desktop app (release build, Windows 11)

| Measurement | Result |
|---|---|
| Executable size | 62.6 MB (bundled SQLite and DuckDB) |
| Cold start to window, 100k-article database | 583 ms first run, ~270 ms warm |
| Idle CPU (10 s sample) | 0 % |
| Memory, GPU renderer (default) | ~165 MB working set |
| Memory, CPU renderer (Settings → CPU renderer) | ~28 MB working set |

GPU memory is dominated by the wgpu renderer, not by the database: an empty database uses the same
amount.

## Not measured yet

- AI request throughput and cost per article against real providers. Only fake local servers
  have been used so far.
- Graph rendering frame times in the running GUI.
- Linux figures.

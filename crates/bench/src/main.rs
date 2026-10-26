//! Performance benchmarks on synthetic data.
//!
//! Usage: juttusihti-bench <articles> <work-dir> [--keep]
//!
//! Builds a database with <articles> synthetic Finnish/English articles in
//! 50 feeds over two years (10% labelled, 5% with entities, 2% linked),
//! then times reader, research and analytics operations. Each query is run
//! several times; the median is reported. Prints a Markdown table and
//! writes `bench-<articles>.json` into the work dir.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use juttusihti_analytics::{Analytics, Bucket, Limits, Window};
use juttusihti_core::analysis::{AnalysisConfig, Depth, Task};
use juttusihti_core::graph::{GraphQuery, NodeRef};
use juttusihti_core::pricing::{Catalog, Provider};
use juttusihti_core::search::SearchQuery;
use juttusihti_core::store::{ListFilter, Store};
use juttusihti_core::{EntityKind, NewArticle, NewFeed, Origin, Page, RelationshipKind, View};

const FEEDS: usize = 50;
const RUNS: usize = 7;
const END: i64 = 1_792_886_400; // 2026-10-25T00:00:00Z
const SPAN: i64 = 2 * 365 * 86_400;

const FI: &[&str] = &[
    "hallitus",
    "eduskunta",
    "talous",
    "työttömyysturva",
    "sää",
    "lumisade",
    "kunta",
    "sairaala",
    "koulu",
    "vaalit",
    "energia",
    "sähkön",
    "hinta",
    "inflaatio",
    "pankki",
    "yritys",
    "tutkimus",
    "ilmasto",
    "liikenne",
    "raitiotie",
    "Helsinki",
    "Tampere",
    "Oulu",
    "Lappi",
    "poliisi",
    "oikeus",
    "päätös",
    "esitys",
    "kritiikki",
    "neuvottelut",
];
const EN: &[&str] = &[
    "government",
    "parliament",
    "economy",
    "unemployment",
    "weather",
    "snow",
    "council",
    "hospital",
    "school",
    "election",
    "energy",
    "electricity",
    "price",
    "inflation",
    "bank",
    "company",
    "research",
    "climate",
    "traffic",
    "tram",
    "police",
    "court",
    "decision",
    "proposal",
    "criticism",
    "negotiations",
    "Finland",
    "Europe",
    "market",
    "union",
];

/// Small deterministic PRNG (xorshift64*), so runs are reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn words(&mut self, n: usize) -> String {
        let vocab = if self.below(2) == 0 { FI } else { EN };
        (0..n).map(|_| vocab[self.below(vocab.len())]).collect::<Vec<_>>().join(" ")
    }
}

struct Results {
    rows: Vec<(String, String)>,
}

impl Results {
    fn add(&mut self, name: &str, value: String) {
        println!("| {name} | {value} |");
        self.rows.push((name.into(), value));
    }

    fn time(&mut self, name: &str, mut f: impl FnMut()) {
        let mut times: Vec<Duration> = (0..RUNS)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed()
            })
            .collect();
        times.sort();
        self.add(name, fmt(times[RUNS / 2]));
    }
}

fn fmt(d: Duration) -> String {
    if d.as_secs_f64() >= 1.0 {
        format!("{:.2} s", d.as_secs_f64())
    } else {
        format!("{:.1} ms", d.as_secs_f64() * 1000.0)
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n: usize = args
        .get(1)
        .and_then(|s| s.replace('_', "").parse().ok())
        .expect("usage: juttusihti-bench <articles> <work-dir> [--keep]");
    let dir = PathBuf::from(args.get(2).expect("work dir"));
    let keep = args.iter().any(|a| a == "--keep");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("bench-{n}.db"));
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }

    println!("## {n} articles\n\n| Measurement | Result |\n|---|---|");
    let mut res = Results { rows: Vec::new() };

    // ---- build ----------------------------------------------------------------
    let mut store = Store::open(&path).unwrap();
    let feeds: Vec<i64> = (0..FEEDS)
        .map(|i| {
            store
                .add_feed(&NewFeed {
                    url: format!("https://feed{i}.example/rss"),
                    title: format!("Feed {i:02}"),
                    ..Default::default()
                })
                .unwrap()
        })
        .collect();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let started = Instant::now();
    let batch = 2_000;
    let mut made = 0;
    while made < n {
        let take = batch.min(n - made);
        let feed = feeds[(made / batch) % FEEDS];
        let entries: Vec<NewArticle> = (0..take)
            .map(|j| {
                let i = made + j;
                let title_len = 5 + rng.below(6);
                let summary_len = 25 + rng.below(30);
                NewArticle {
                    guid: format!("g{i}"),
                    url: Some(format!("https://news.example/{i}")),
                    title: rng.words(title_len),
                    summary: Some(format!("<p>{}</p>", rng.words(summary_len))),
                    content: (rng.below(5) == 0).then(|| rng.words(250)),
                    author: None,
                    published_at: (rng.below(50) != 0).then(|| END - (rng.next() % SPAN as u64) as i64),
                }
            })
            .collect();
        store.ingest(feed, &entries).unwrap();
        made += take;
    }
    let ingest = started.elapsed();
    res.add(
        "Ingest (batches of 2,000)",
        format!("{} ({:.0} articles/s)", fmt(ingest), n as f64 / ingest.as_secs_f64()),
    );

    // Research data: labels on 10%, entities on 5%, links on 2%.
    let started = Instant::now();
    let labels: Vec<i64> = ["Politiikka", "Talous", "Sää", "Energia", "Terveys", "Liikenne", "Ilmasto", "Rikos"]
        .iter()
        .map(|l| store.create_label(l).unwrap())
        .collect();
    let entities: Vec<i64> = ["Helsinki", "Tampere", "Eduskunta", "Kela", "Fortum", "HUS"]
        .iter()
        .enumerate()
        .map(|(i, e)| {
            store.resolve_entity(if i < 2 { EntityKind::Place } else { EntityKind::Organization }, e).unwrap()
        })
        .collect();
    store.conn().execute_batch("BEGIN").unwrap();
    let ids: Vec<i64> = {
        let mut stmt = store.conn().prepare("SELECT id FROM articles ORDER BY id").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    };
    for (k, id) in ids.iter().enumerate() {
        if k % 10 == 0 {
            let origin = if k % 20 == 0 { Origin::User } else { Origin::Ai };
            store.tag_article(*id, labels[rng.below(labels.len())], origin, None).unwrap();
        }
        if k % 20 == 1 {
            store.link_entity(*id, entities[rng.below(entities.len())], "mentioned", None, Origin::User, None).unwrap();
            store.link_entity(*id, entities[rng.below(entities.len())], "subject", None, Origin::User, None).unwrap();
        }
        if k % 50 == 2 && k + 7 < ids.len() {
            let _ = store.relate(*id, ids[k + 7], RelationshipKind::Related, Origin::Ai, None, Some(0.7), None);
        }
    }
    store.conn().execute_batch("COMMIT").unwrap();
    res.add("Research data (labels/entities/links)", fmt(started.elapsed()));
    store.conn().execute_batch("PRAGMA wal_checkpoint(TRUNCATE); ANALYZE;").unwrap();
    drop(store);
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    res.add(
        "Database size",
        format!("{:.1} MB ({:.0} bytes/article)", size as f64 / 1_048_576.0, size as f64 / n as f64),
    );

    // ---- reader -------------------------------------------------------------------
    let t = Instant::now();
    let store = Store::open(&path).unwrap();
    res.add("Open existing database", fmt(t.elapsed()));
    let page = Page::first(50);
    res.time("List: All, first page", || {
        store.list_articles(ListFilter::default(), page).unwrap();
    });
    res.time("List: Unread, page 100", || {
        let p = Page { offset: 50 * 99, limit: 50 };
        store.list_articles(ListFilter { view: View::Unread, ..Default::default() }, p).unwrap();
    });
    res.time("List: one feed, first page", || {
        store.list_articles(ListFilter { feed_id: Some(feeds[3]), ..Default::default() }, page).unwrap();
    });
    let q = |text: &str| SearchQuery { text: text.into(), ..Default::default() };
    for (name, query) in [
        ("Search: common word", q("hallitus")),
        ("Search: prefix", q("työttömyys*")),
        ("Search: phrase", q("\"energy price\"")),
        ("Search: boolean", q("(sää OR weather) -snow")),
        ("Search: NEAR", q("NEAR(government decision, 5)")),
        (
            "Search: word + feed + 30-day range",
            SearchQuery {
                text: "talous".into(),
                feed_ids: vec![feeds[1], feeds[2]],
                from: Some(END - 30 * 86_400),
                to: Some(END),
                ..Default::default()
            },
        ),
        ("Filter: label (confirmed)", SearchQuery { label_ids: vec![labels[0]], ..Default::default() }),
        ("Filter: entity", SearchQuery { entity_ids: vec![entities[2]], ..Default::default() }),
        ("Filter: 7-day range", SearchQuery { from: Some(END - 7 * 86_400), to: Some(END), ..Default::default() }),
    ] {
        res.time(name, || {
            store.search(&query, page).unwrap();
        });
    }
    let some_article = ids[ids.len() / 2 + 2];
    res.time("Article relationships", || {
        store.relationships(some_article).unwrap();
    });

    // ---- research -----------------------------------------------------------------
    let config = AnalysisConfig {
        scope: SearchQuery { from: Some(END - 30 * 86_400), to: Some(END), ..Default::default() },
        range_label: "bench".into(),
        provider: Provider::Anthropic,
        model: "claude-haiku-5-5".into(),
        depth: Depth::HeadlinesAndDescriptions,
        tasks: [Task::Labels, Task::Related, Task::Summary].into_iter().collect::<BTreeSet<_>>(),
        limits: Default::default(),
        max_articles: 1_000,
        reuse_cached: true,
        use_batch: false,
        relevance_criteria: None,
    };
    let mut scope = None;
    res.time("Analysis scope: 30 days, max 1,000 (dedup, cache, candidates)", || {
        scope = Some(store.analysis_scope(&config).unwrap());
    });
    let scope = scope.unwrap();
    res.add(
        "  matched / distinct / candidate pairs",
        format!("{} / {} / {}", scope.stats.matched, scope.stats.distinct, scope.candidates.len()),
    );
    let catalog = Catalog::builtin();
    res.time("Estimate for that scope", || {
        juttusihti_core::estimate::estimate(
            &config,
            &scope,
            8,
            &catalog,
            chrono::NaiveDate::from_ymd_opt(2026, 10, 25).unwrap(),
        );
    });
    res.time("Timeline: 30 days (≤2,000 articles)", || {
        store
            .timeline(&SearchQuery { from: Some(END - 30 * 86_400), to: Some(END), ..Default::default() }, 2_000)
            .unwrap();
    });
    let linked = ids[2];
    let mut graph = None;
    res.time("Graph: 2 hops around a linked article", || {
        graph = Some(
            store
                .graph(&GraphQuery {
                    seeds: vec![NodeRef::Article(linked)],
                    hops: 2,
                    max_nodes: 150,
                    ..Default::default()
                })
                .unwrap(),
        );
    });
    let g = graph.unwrap();
    let edges: Vec<(usize, usize)> = g.edges.iter().map(|e| (e.a, e.b)).collect();
    res.time(&format!("Graph layout ({} nodes)", g.nodes.len()), || {
        juttusihti_core::graph::layout(g.nodes.len(), &edges, 300);
    });
    let full: Vec<(usize, usize)> = (0..150).flat_map(|i| [(i, (i + 1) % 150), (i, (i * 7 + 3) % 150)]).collect();
    res.time("Graph layout (150 nodes, cap)", || {
        juttusihti_core::graph::layout(150, &full, 300);
    });

    // ---- DuckDB -----------------------------------------------------------------------
    let ro = juttusihti_core::db::open_read_only(&path).unwrap();
    let t = Instant::now();
    let analytics = Analytics::snapshot(&ro, &Limits::default()).unwrap();
    res.add("DuckDB snapshot (copy from SQLite)", fmt(t.elapsed()));
    let w = Window { from: END - 365 * 86_400, to: END, utc_offset_secs: 10_800 };
    res.time("Report: volume by source, 1 year/week", || {
        analytics.volume_by_source(w, Bucket::Week).unwrap();
    });
    res.time("Report: topic evolution, 1 year/month", || {
        analytics.label_trend(w, Bucket::Month, 6).unwrap();
    });
    res.time("Report: entity co-occurrence", || {
        analytics.entity_cooccurrence(w, 2, 25).unwrap();
    });
    res.time("Report: labels by source", || {
        analytics.labels_by_source(w, 8).unwrap();
    });
    drop(analytics);
    drop(ro);
    drop(store);

    let json: serde_json::Value = res
        .rows
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
        .collect::<serde_json::Map<_, _>>()
        .into();
    std::fs::write(dir.join(format!("bench-{n}.json")), serde_json::to_string_pretty(&json).unwrap()).unwrap();
    if !keep {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
    println!();
}

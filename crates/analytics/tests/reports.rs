use std::time::Duration;

use juttusihti_analytics::{Analytics, AnalyticsWorker, Bucket, Error, Limits, Window};
use juttusihti_core::store::Store;
use juttusihti_core::{EntityKind, NewArticle, NewFeed, Origin};

const OCT1: i64 = 1_790_812_800; // 2026-10-01T00:00:00Z
const DAY: i64 = 86_400;

fn article(guid: &str, title: &str, at: i64) -> NewArticle {
    NewArticle {
        guid: guid.into(),
        url: Some(format!("https://x.example/{guid}")),
        title: title.into(),
        summary: None,
        content: None,
        author: None,
        published_at: Some(at),
    }
}

fn seed(store: &mut Store) {
    let a = store
        .add_feed(&NewFeed { url: "https://a.example/rss".into(), title: "Alpha".into(), ..Default::default() })
        .unwrap();
    let b = store
        .add_feed(&NewFeed { url: "https://b.example/rss".into(), title: "Beta".into(), ..Default::default() })
        .unwrap();
    store
        .ingest(a, &[article("1", "one", OCT1), article("2", "two", OCT1 + 3600), article("3", "three", OCT1 + DAY)])
        .unwrap();
    // Late on Oct 1 UTC, but Oct 2 in UTC+3.
    store.ingest(b, &[article("4", "four", OCT1 + DAY - 1800), article("5", "five", OCT1 + 40 * DAY)]).unwrap();

    let ids: Vec<i64> = store
        .list_articles(Default::default(), juttusihti_core::Page::first(10))
        .unwrap()
        .0
        .iter()
        .map(|a| a.id)
        .collect();
    let label = store.create_label("Talous").unwrap();
    // ids[0] is article five, outside the report window; use the others.
    store.tag_article(ids[1], label, Origin::User, None).unwrap();
    store.tag_article(ids[2], label, Origin::Ai, None).unwrap();
    store.tag_article(ids[0], label, Origin::User, None).unwrap();
    let rejected = store.create_label("Urheilu").unwrap();
    store.tag_article(ids[2], rejected, Origin::Ai, None).unwrap();
    store.decide_label(ids[2], rejected, juttusihti_core::Status::Rejected).unwrap();

    let p = store.resolve_entity(EntityKind::Person, "Sanna Esimerkki").unwrap();
    let o = store.resolve_entity(EntityKind::Organization, "Eduskunta").unwrap();
    for id in &ids[1..4] {
        store.link_entity(*id, p, "mentioned", None, Origin::User, None).unwrap();
        store.link_entity(*id, o, "mentioned", None, Origin::User, None).unwrap();
    }
    store
        .conn()
        .execute_batch(
            "INSERT INTO usage_ledger(provider, model, operation, input_tokens, output_tokens, cost_microunits, created_at)
             VALUES ('anthropic', 'm1', 'labels', 1000, 200, 5000, 0),
                    ('anthropic', 'm1', 'labels', 500, 100, 2500, 0),
                    ('openai', 'm2', 'labels', 800, 50, NULL, 0);",
        )
        .unwrap();
}

fn window(offset: i64) -> Window {
    Window { from: OCT1, to: OCT1 + 31 * DAY, utc_offset_secs: offset }
}

#[test]
fn reports_over_snapshot() {
    let mut store = Store::open_in_memory().unwrap();
    seed(&mut store);
    let a = Analytics::snapshot(store.conn(), &Limits::default()).unwrap();
    assert_eq!(a.stats().articles, 5);

    let utc = a.volume_by_source(window(0), Bucket::Day).unwrap();
    let summary: Vec<(String, i64, i64)> =
        utc.iter().map(|r| (r.feed_title.clone(), r.bucket_start, r.articles)).collect();
    assert_eq!(
        summary,
        vec![("Alpha".into(), OCT1, 2), ("Beta".into(), OCT1, 1), ("Alpha".into(), OCT1 + DAY, 1)],
        "article 5 is outside the half-open window"
    );
    // In UTC+3 the late Beta article falls on Oct 2 local; buckets start at local midnight.
    let helsinki = a.volume_by_source(window(3 * 3600), Bucket::Day).unwrap();
    let beta = helsinki.iter().find(|r| r.feed_title == "Beta").unwrap();
    assert_eq!(beta.bucket_start, OCT1 + DAY - 3 * 3600);

    let monthly = a.volume_by_source(window(0), Bucket::Month).unwrap();
    assert_eq!(monthly.iter().map(|r| r.articles).sum::<i64>(), 4);

    let labels = a.label_distribution(window(0)).unwrap();
    assert_eq!(labels.len(), 1, "rejected labels are excluded");
    assert_eq!((labels[0].confirmed, labels[0].suggested), (1, 1));

    let co = a.entity_cooccurrence(window(0), 2, 10).unwrap();
    assert_eq!(co.len(), 1);
    assert_eq!(co[0].articles, 3);

    let usage = a.usage_by_model().unwrap();
    assert_eq!(usage[0].cost_microunits, Some(7500));
    assert_eq!(usage[0].input_tokens, 1500);
    assert_eq!(usage[1].cost_microunits, None, "unknown cost is never summed as zero");

    let stories = a.distinct_stories(window(0)).unwrap();
    assert_eq!(stories[0], ("Alpha".into(), 3, 3));
}

#[test]
fn worker_snapshots_file_database_and_sees_refreshes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("j.db");
    let mut store = Store::open(&path).unwrap();
    seed(&mut store);
    let worker = AnalyticsWorker::spawn(path.clone(), Limits::default()).unwrap();
    let stats = worker.refresh().unwrap().recv().unwrap().unwrap();
    assert_eq!(stats.articles, 5);

    // The snapshot does not change until refreshed, and writes are not blocked meanwhile.
    let feed = store.feeds().unwrap()[0].id;
    store.ingest(feed, &[article("6", "six", OCT1 + 2 * DAY)]).unwrap();
    let n = worker.run(|a| Ok(a.stats().articles)).unwrap().recv().unwrap().unwrap();
    assert_eq!(n, 5);
    assert_eq!(worker.refresh().unwrap().recv().unwrap().unwrap().articles, 6);
}

#[test]
fn worker_queries_can_be_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("j.db");
    let mut store = Store::open(&path).unwrap();
    seed(&mut store);
    let worker = AnalyticsWorker::spawn(path, Limits { threads: 1, ..Default::default() }).unwrap();
    worker.refresh().unwrap().recv().unwrap().unwrap();
    let rx = worker.run(|a| a.heavy_query_for_tests()).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    worker.cancel_handle().cancel();
    let result = rx.recv_timeout(Duration::from_secs(30)).expect("query should stop after interrupt");
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    // The worker keeps serving after a cancellation.
    assert_eq!(worker.run(|a| Ok(a.stats().articles)).unwrap().recv().unwrap().unwrap(), 5);
}

#[test]
fn missing_database_reports_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let worker = AnalyticsWorker::spawn(dir.path().join("nope.db"), Limits::default()).unwrap();
    assert!(worker.refresh().unwrap().recv().unwrap().is_err());
}

//! Full-article text: candidate selection, separate storage, search index
//! sync, reading view and AI input.

use std::collections::BTreeSet;

use juttusihti_core::analysis::{AnalysisConfig, ClickbaitCheck, Depth, Task};
use juttusihti_core::budget::Limits;
use juttusihti_core::extract::{ExtractStatus, Extraction, MIN_EXTRACT_CHARS, TEASER_MAX_CHARS};
use juttusihti_core::search::SearchQuery;
use juttusihti_core::store::Store;
use juttusihti_core::*;

const FULL: &str = "Satamajohtaja kertoo, että laajennus viivästyy kallioperän vuoksi. ";

fn entry(guid: &str, title: &str, summary: &str) -> NewArticle {
    NewArticle {
        guid: guid.into(),
        url: Some(format!("https://news.example/{guid}")),
        title: title.into(),
        summary: Some(summary.into()),
        content: None,
        author: None,
        published_at: Some(now() - 3_600),
    }
}

fn full_text() -> String {
    FULL.repeat(MIN_EXTRACT_CHARS / FULL.len() + 2).trim().to_string()
}

/// A feed with full-text fetching on: one teaser, one long entry, one without a URL.
fn seeded() -> (Store, FeedId) {
    let mut s = Store::open_in_memory().unwrap();
    let feed = s.add_feed(&NewFeed { url: "https://news.example/rss".into(), ..Default::default() }).unwrap();
    let mut no_url = entry("3", "Ilman linkkiä", "Lyhyt.");
    no_url.url = None;
    s.ingest(
        feed,
        &[
            entry("1", "Sataman laajennus viivästyy", "Laajennus myöhästyy. Lue lisää."),
            entry("2", "Pitkä juttu", &"Koko teksti on jo syötteessä. ".repeat(TEASER_MAX_CHARS / 20)),
            no_url,
        ],
    )
    .unwrap();
    (s, feed)
}

fn id_of(s: &Store, guid: &str) -> ArticleId {
    s.conn().query_row("SELECT id FROM articles WHERE guid = ?1", [guid], |r| r.get(0)).unwrap()
}

fn hits(s: &Store, q: &str) -> usize {
    s.search(&SearchQuery { text: q.into(), ..Default::default() }, Page::first(10)).unwrap().items.len()
}

fn fts_ok(s: &Store) {
    s.conn().execute("INSERT INTO articles_fts(articles_fts) VALUES ('integrity-check')", []).unwrap();
}

#[test]
fn feeds_are_off_by_default_and_only_teasers_are_candidates() {
    let (s, feed) = seeded();
    assert!(!s.feed(feed).unwrap().fetch_full_text);
    assert!(s.full_text_candidates(feed, 10).unwrap().is_empty(), "off by default");

    s.set_feed_full_text(feed, true).unwrap();
    assert!(s.feed(feed).unwrap().fetch_full_text);
    let c = s.full_text_candidates(feed, 10).unwrap();
    assert_eq!(c.len(), 1, "long entries and entries without a URL are skipped: {c:?}");
    assert_eq!(c[0].url, "https://news.example/1");

    // A tried article, even a failed one, is not fetched again.
    s.save_extract(c[0].article_id, &c[0].url, Extraction::Failed("server returned 500".into())).unwrap();
    assert!(s.full_text_candidates(feed, 10).unwrap().is_empty());
    let stored = s.article_extract(c[0].article_id).unwrap().unwrap();
    assert_eq!(stored.status, ExtractStatus::Failed);
    assert_eq!(stored.error.as_deref(), Some("server returned 500"));

    s.set_feed_full_text(feed, false).unwrap();
    s.clear_extract(c[0].article_id).unwrap();
    assert!(s.full_text_candidates(feed, 10).unwrap().is_empty(), "switching off stops fetching");
}

#[test]
fn old_entries_are_not_crawled() {
    let mut s = Store::open_in_memory().unwrap();
    let feed = s.add_feed(&NewFeed { url: "https://news.example/rss".into(), ..Default::default() }).unwrap();
    let mut old = entry("old", "Vanha juttu", "Lyhyt.");
    old.published_at = Some(now() - 60 * 86_400);
    s.ingest(feed, &[old]).unwrap();
    s.set_feed_full_text(feed, true).unwrap();
    assert!(s.full_text_candidates(feed, 10).unwrap().is_empty());
}

#[test]
fn extracted_text_is_stored_apart_and_used_for_reading_and_search() {
    let (s, _) = seeded();
    let id = id_of(&s, "1");
    assert_eq!(hits(&s, "kallioperän"), 0);

    let status = s.save_extract(id, "https://news.example/1?page=final", Extraction::Text(full_text())).unwrap();
    assert_eq!(status, ExtractStatus::Ok);

    // The feed's own content is untouched.
    let a = s.article(id).unwrap();
    assert_eq!(a.summary.as_deref(), Some("Laajennus myöhästyy. Lue lisää."));
    assert_eq!(a.content, None);
    // Reading view prefers the extracted text.
    assert!(s.article_text(id).unwrap().starts_with("Satamajohtaja kertoo"));
    // Search finds it, also through the content: column filter, with a snippet.
    assert_eq!(hits(&s, "kallioperän"), 1);
    assert_eq!(hits(&s, "content:kallioperän"), 1);
    assert_eq!(hits(&s, "title:kallioperän"), 0);
    let r = s.search(&SearchQuery { text: "kallioperän".into(), ..Default::default() }, Page::first(5)).unwrap();
    assert!(r.items[0].snippet.as_deref().unwrap().contains("kallioperän"));
    fts_ok(&s);

    // Replacing, then clearing the extract keeps the index in sync.
    s.save_extract(id, "https://news.example/1", Extraction::Text(format!("Uusi versio: {}", full_text()))).unwrap();
    assert_eq!(hits(&s, "versio"), 1);
    fts_ok(&s);
    s.clear_extract(id).unwrap();
    assert_eq!(hits(&s, "kallioperän"), 0);
    assert_eq!(hits(&s, "laajennus"), 1, "the feed text stays indexed");
    assert_eq!(s.article_text(id).unwrap(), "Laajennus myöhästyy. Lue lisää.");
    fts_ok(&s);
}

#[test]
fn feed_updates_and_deletes_keep_the_index_in_sync() {
    let (mut s, feed) = seeded();
    let id = id_of(&s, "1");
    s.save_extract(id, "https://news.example/1", Extraction::Text(full_text())).unwrap();

    // The feed edits the entry: both its new text and the extract stay searchable.
    s.ingest(feed, &[entry("1", "Sataman laajennus viivästyy taas", "Päivitetty teaser.")]).unwrap();
    assert_eq!(hits(&s, "päivitetty"), 1);
    assert_eq!(hits(&s, "kallioperän"), 1);
    fts_ok(&s);

    // Retention and unsubscribing remove extracts with their articles.
    assert_eq!(s.apply_retention(0).unwrap(), 3);
    assert_eq!(hits(&s, "kallioperän"), 0);
    let n: i64 = s.conn().query_row("SELECT count(*) FROM article_extracts", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 0);
    fts_ok(&s);

    let (s, feed) = seeded();
    s.save_extract(id_of(&s, "1"), "https://news.example/1", Extraction::Text(full_text())).unwrap();
    s.remove_feed(feed).unwrap();
    assert_eq!(hits(&s, "kallioperän"), 0);
    fts_ok(&s);
}

#[test]
fn text_no_longer_than_the_feed_text_is_not_kept() {
    let (s, _) = seeded();
    let id = id_of(&s, "1");
    assert_eq!(
        s.save_extract(id, "https://news.example/1", Extraction::Text("Lyhyt.".into())).unwrap(),
        ExtractStatus::Empty
    );
    assert_eq!(s.save_extract(id, "https://news.example/1", Extraction::NoText).unwrap(), ExtractStatus::Empty);
    assert_eq!(s.article_extract(id).unwrap().unwrap().text, None);
    assert_eq!(s.article_text(id).unwrap(), "Laajennus myöhästyy. Lue lisää.");
    // An article deleted while its page was being fetched is ignored.
    assert_eq!(s.save_extract(9_999, "https://x.example/", Extraction::NoText).unwrap(), ExtractStatus::Failed);
}

#[test]
fn full_text_analysis_uses_the_extract_and_its_own_cache_key() {
    let (s, _) = seeded();
    let id = id_of(&s, "1");
    let config = |depth| AnalysisConfig {
        scope: SearchQuery::default(),
        range_label: String::new(),
        provider: juttusihti_core::pricing::Provider::Anthropic,
        billing: Default::default(),
        model: "test-model".into(),
        depth,
        tasks: BTreeSet::from([Task::Labels]),
        limits: Limits::default(),
        max_articles: 100,
        reuse_cached: true,
        use_batch: false,
        relevance_criteria: None,
        clickbait: ClickbaitCheck::off(),
        parallel_requests: 1,
    };
    let record = |depth| s.analysis_scope(&config(depth)).unwrap().records.into_iter().find(|r| r.id == id).unwrap();
    let before = record(Depth::FullText);
    assert!(before.body.starts_with("Laajennus myöhästyy"));

    s.save_extract(id, "https://news.example/1", Extraction::Text(full_text())).unwrap();
    let after = record(Depth::FullText);
    assert!(after.body.starts_with("Satamajohtaja kertoo"), "{}", after.body);
    assert_ne!(after.content_hash, before.content_hash, "teaser results must not be reused");
    // Description-depth input and cache keys are unaffected.
    let desc = record(Depth::HeadlinesAndDescriptions);
    assert!(desc.body.starts_with("Laajennus myöhästyy"));
    assert_eq!(desc.content_hash, before.content_hash);
}

#[test]
fn upgrading_a_version_5_database_indexes_existing_articles() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    for sql in [
        include_str!("../migrations/0001_reader.sql"),
        include_str!("../migrations/0002_research.sql"),
        include_str!("../migrations/0003_relevance.sql"),
        include_str!("../migrations/0004_list_indexes.sql"),
        include_str!("../migrations/0005_headlines.sql"),
    ] {
        conn.execute_batch(sql).unwrap();
    }
    conn.pragma_update(None, "user_version", 5).unwrap();
    conn.execute_batch(
        "INSERT INTO feeds(id, url, title, created_at) VALUES (1, 'https://news.example/rss', 'Uutiset', 0);
         INSERT INTO articles(feed_id, guid, title, summary_text, fetched_at, effective_at, dedup_key, content_hash)
         VALUES (1, 'g', 'Raitiotie etenee', 'Kaupunginvaltuusto hyväksyi linjauksen.', 0, 0, 'k', 'h');",
    )
    .unwrap();

    juttusihti_core::db::migrate(&conn).unwrap();
    let v: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
    assert_eq!(v, juttusihti_core::db::SCHEMA_VERSION);
    let n: i64 = conn
        .query_row("SELECT count(*) FROM articles_fts WHERE articles_fts MATCH 'kaupunginvaltuusto'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "existing articles are re-indexed");
    let off: bool = conn.query_row("SELECT fetch_full_text FROM feeds WHERE id = 1", [], |r| r.get(0)).unwrap();
    assert!(!off);
    conn.execute("INSERT INTO articles_fts(articles_fts) VALUES ('integrity-check')", []).unwrap();
}

#[test]
fn list_rows_flag_articles_with_full_text() {
    let (s, feed) = seeded();
    let id = id_of(&s, "1");
    let flagged = |q: &str| -> Vec<(ArticleId, bool)> {
        let filter = juttusihti_core::store::ListFilter { feed_id: Some(feed), ..Default::default() };
        let items = if q.is_empty() {
            s.list_articles(filter, Page::first(10)).unwrap().0
        } else {
            s.search(&SearchQuery { text: q.into(), ..Default::default() }, Page::first(10)).unwrap().items
        };
        items.into_iter().map(|a| (a.id, a.has_full_text)).collect()
    };
    assert!(flagged("").iter().all(|(_, f)| !f));

    s.save_extract(id, "https://news.example/1", Extraction::Failed("403".into())).unwrap();
    assert!(flagged("").iter().all(|(_, f)| !f), "a failed attempt is not full text");

    s.save_extract(id, "https://news.example/1", Extraction::Text(full_text())).unwrap();
    for q in ["", "sataman", "kallioperän"] {
        let rows = flagged(q);
        assert!(rows.contains(&(id, true)), "{q:?}: {rows:?}");
        assert!(rows.iter().filter(|(_, f)| *f).count() == 1, "{q:?}: {rows:?}");
    }
}

#[test]
fn any_linked_article_can_be_fetched_on_request() {
    let (s, feed) = seeded();
    // The feed setting is off and entry 2 is not a teaser: still a valid target.
    let long = id_of(&s, "2");
    let c = s.full_text_target(long).unwrap();
    assert_eq!(c.url, "https://news.example/2");
    assert!(s.full_text_candidates(feed, 10).unwrap().is_empty());
    assert!(matches!(s.full_text_target(id_of(&s, "3")), Err(Error::Invalid(_))), "no link");
    assert!(matches!(s.full_text_target(9_999), Err(Error::NotFound)));
}

#[test]
fn a_failed_refetch_keeps_the_earlier_text() {
    let (s, _) = seeded();
    let id = id_of(&s, "1");
    s.save_extract(id, "https://news.example/1", Extraction::Text(full_text())).unwrap();
    let failed = s.save_extract(id, "https://news.example/1", Extraction::Failed("server returned 503".into()));
    assert_eq!(failed.unwrap(), ExtractStatus::Failed, "the attempt itself is reported");
    assert_eq!(s.save_extract(id, "https://news.example/1", Extraction::NoText).unwrap(), ExtractStatus::Empty);
    let kept = s.article_extract(id).unwrap().unwrap();
    assert_eq!(kept.status, ExtractStatus::Ok);
    assert_eq!(kept.text.as_deref(), Some(full_text().as_str()));
    assert_eq!(hits(&s, "kallioperän"), 1);
}

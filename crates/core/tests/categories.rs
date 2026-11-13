use juttusihti_core::search::SearchQuery;
use juttusihti_core::store::Store;
use juttusihti_core::*;

fn entry(guid: &str, terms: &[&str]) -> NewArticle {
    NewArticle {
        guid: guid.into(),
        url: Some(format!("https://news.example/{guid}")),
        title: format!("Title {guid}"),
        summary: Some("Body".into()),
        content: None,
        author: None,
        published_at: Some(1_790_812_800),
        terms: terms.iter().map(|t| NewTerm { name: (*t).into(), scheme: String::new() }).collect(),
    }
}

fn feed(s: &mut Store, url: &str) -> FeedId {
    s.add_feed(&NewFeed { url: url.into(), title: url.into(), ..Default::default() }).unwrap()
}

fn count(s: &Store, table: &str) -> i64 {
    s.conn().query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap()
}

#[test]
fn ingest_links_terms_idempotently() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    let e = entry("1", &["AI", "Politiikka"]);
    s.ingest(f, std::slice::from_ref(&e)).unwrap();
    s.ingest(f, std::slice::from_ref(&e)).unwrap();
    assert_eq!(count(&s, "feed_terms"), 2);
    assert_eq!(count(&s, "article_terms"), 2);
}

#[test]
fn same_word_in_two_feeds_is_two_terms() {
    let mut s = Store::open_in_memory().unwrap();
    let f1 = feed(&mut s, "https://a.example/rss");
    let f2 = feed(&mut s, "https://b.example/rss");
    s.ingest(f1, &[entry("1", &["AI"])]).unwrap();
    s.ingest(f2, &[entry("2", &["AI"])]).unwrap();
    assert_eq!(count(&s, "feed_terms"), 2);
}

#[test]
fn unchanged_article_gains_terms_on_refresh() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &[])]).unwrap();
    assert_eq!(count(&s, "article_terms"), 0);
    let st = s.ingest(f, &[entry("1", &["AI"])]).unwrap();
    assert_eq!(st.unchanged, 1);
    assert_eq!(count(&s, "article_terms"), 1);
}

#[test]
fn new_terms_start_unsorted() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["AI"])]).unwrap();
    let n: i64 = s
        .conn()
        .query_row("SELECT COUNT(*) FROM feed_terms WHERE label_id IS NULL AND ignored = 0", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn removing_feed_removes_its_terms() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["AI"])]).unwrap();
    s.remove_feed(f).unwrap();
    assert_eq!(count(&s, "feed_terms"), 0);
    assert_eq!(count(&s, "article_terms"), 0);
}

fn entry_scheme(guid: &str, name: &str, scheme: &str) -> NewArticle {
    let mut e = entry(guid, &[]);
    e.terms = vec![NewTerm { name: name.into(), scheme: scheme.into() }];
    e
}

#[test]
fn term_identity_is_normalized() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["AI"]), entry("2", &[" ai "])]).unwrap();
    assert_eq!(count(&s, "feed_terms"), 1);
    assert_eq!(count(&s, "article_terms"), 2);
}

#[test]
fn same_name_different_schemes_are_two_terms() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry_scheme("1", "AI", ""), entry_scheme("2", "AI", "urn:x")]).unwrap();
    assert_eq!(count(&s, "feed_terms"), 2);
}

fn map_term(s: &Store, name: &str, label: LabelId) {
    s.conn().execute("UPDATE feed_terms SET label_id = ?1 WHERE name = ?2", rusqlite::params![label, name]).unwrap();
}

fn article_id(s: &Store, guid: &str) -> ArticleId {
    s.conn().query_row("SELECT id FROM articles WHERE guid = ?1", [guid], |r| r.get(0)).unwrap()
}

fn by_label(s: &Store, l: LabelId) -> u64 {
    let q = SearchQuery { label_ids: vec![l], ..Default::default() };
    s.search(&q, Page { limit: 50, offset: 0 }).unwrap().total
}

fn group(s: &Store) -> i64 {
    s.conn()
        .execute("INSERT INTO category_groups(name, normalized, position, created_at) VALUES ('Aihe','aihe',0,0)", [])
        .unwrap();
    s.conn().last_insert_rowid()
}

#[test]
fn mapped_term_puts_article_in_category() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["AI"]), entry("2", &["Muu"])]).unwrap();
    let l = s.create_label("Tekoäly").unwrap();
    assert_eq!(by_label(&s, l), 0);
    map_term(&s, "AI", l);
    assert_eq!(by_label(&s, l), 1);
}

#[test]
fn group_filter_returns_each_article_once() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["AI", "Robotit"]), entry("2", &["Muu"])]).unwrap();
    let a = s.create_label("Tekoäly").unwrap();
    let b = s.create_label("Robotiikka").unwrap();
    let g = group(&s);
    s.conn().execute("UPDATE labels SET group_id = ?1", [g]).unwrap();
    map_term(&s, "AI", a);
    map_term(&s, "Robotit", b);
    s.tag_article(article_id(&s, "1"), a, Origin::User, None).unwrap();
    let q = SearchQuery { group_ids: vec![g], ..Default::default() };
    let r = s.search(&q, Page { limit: 50, offset: 0 }).unwrap();
    assert_eq!(r.total, 1);
    assert_eq!(r.items.len(), 1);
}

#[test]
fn rejection_beats_mapping() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["AI"])]).unwrap();
    let l = s.create_label("Tekoäly").unwrap();
    map_term(&s, "AI", l);
    s.untag_article(article_id(&s, "1"), l).unwrap();
    assert_eq!(by_label(&s, l), 0);
}

#[test]
fn rejection_survives_refresh() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    let e = entry("1", &["AI"]);
    s.ingest(f, std::slice::from_ref(&e)).unwrap();
    let l = s.create_label("Tekoäly").unwrap();
    map_term(&s, "AI", l);
    s.untag_article(article_id(&s, "1"), l).unwrap();
    s.ingest(f, std::slice::from_ref(&e)).unwrap();
    assert_eq!(by_label(&s, l), 0);
}

#[test]
fn article_labels_shows_term_derived() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["AI"])]).unwrap();
    let l = s.create_label("Tekoäly").unwrap();
    map_term(&s, "AI", l);
    let a = article_id(&s, "1");
    let got = s.article_labels(a).unwrap();
    assert_eq!(got.len(), 1);
    assert!(got[0].via_terms);
    assert_eq!(got[0].origin, Origin::User);
    assert_eq!(got[0].status, Status::Confirmed);
    s.untag_article(a, l).unwrap();
    assert!(s.article_labels(a).unwrap().iter().all(|l| !l.via_terms && l.status == Status::Rejected));
}

#[test]
fn merge_moves_term_mappings() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["AI"])]).unwrap();
    let from = s.create_label("Tekoäly").unwrap();
    let into = s.create_label("Teknologia").unwrap();
    let g = group(&s);
    s.conn().execute("UPDATE labels SET group_id = ?1 WHERE id = ?2", rusqlite::params![g, into]).unwrap();
    map_term(&s, "AI", from);
    s.merge_labels(from, into).unwrap();
    let m: i64 = s.conn().query_row("SELECT label_id FROM feed_terms WHERE name = 'AI'", [], |r| r.get(0)).unwrap();
    assert_eq!(m, into);
    let gid: i64 = s.conn().query_row("SELECT group_id FROM labels WHERE id = ?1", [into], |r| r.get(0)).unwrap();
    assert_eq!(gid, g);
}

#[test]
fn delete_category_returns_terms_to_unsorted() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["AI"])]).unwrap();
    let l = s.create_label("Tekoäly").unwrap();
    map_term(&s, "AI", l);
    s.delete_label(l).unwrap();
    let n: i64 = s
        .conn()
        .query_row("SELECT COUNT(*) FROM feed_terms WHERE label_id IS NULL AND ignored = 0", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

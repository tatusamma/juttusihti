use juttusihti_core::search::SearchQuery;
use juttusihti_core::store::Store;
use juttusihti_core::*;

/// 2026-10-01T00:00:00Z
const OCT1: i64 = 1_790_812_800;
const DAY: i64 = 86_400;

fn entry(guid: &str, title: &str, summary: &str, published_at: i64) -> NewArticle {
    NewArticle {
        guid: guid.into(),
        url: Some(format!("https://news.example/{guid}")),
        title: title.into(),
        summary: Some(summary.into()),
        content: None,
        author: None,
        published_at: Some(published_at),
    }
}

fn seeded() -> (Store, FeedId, Vec<ArticleId>) {
    let mut s = Store::open_in_memory().unwrap();
    let feed = s
        .add_feed(&NewFeed { url: "https://fi.example/rss".into(), title: "Suomi".into(), ..Default::default() })
        .unwrap();
    s.ingest(
        feed,
        &[
            entry("1", "Hallitus esittää muutoksia", "Esitys koskee päivärahaa.", OCT1),
            entry("2", "Lumisade jatkuu Lapissa", "Ilmatieteen laitos varoittaa.", OCT1 + DAY),
            entry("3", "Raitiotie etenee", "Valtuusto äänesti.", OCT1 + 2 * DAY),
        ],
    )
    .unwrap();
    let ids = article_ids(&s);
    (s, feed, ids)
}

fn article_ids(s: &Store) -> Vec<ArticleId> {
    let mut stmt = s.conn().prepare("SELECT id FROM articles ORDER BY id").unwrap();
    stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
}

fn integrity(s: &Store) {
    s.conn()
        .execute("INSERT INTO articles_fts(articles_fts, rank) VALUES ('integrity-check', 1)", [])
        .expect("FTS index matches its content");
}

fn found(s: &Store, q: &str) -> Vec<ArticleId> {
    let q = SearchQuery { text: q.into(), ..Default::default() };
    s.search(&q, Page::first(50)).unwrap().items.into_iter().map(|a| a.id).collect()
}

fn set_fetched_at(s: &Store, id: ArticleId, at: i64) {
    s.conn().execute("UPDATE article_fulltext SET fetched_at = ?1 WHERE article_id = ?2", [at, id]).unwrap();
}

#[test]
fn fetched_text_is_searchable_and_survives_refresh() {
    let (mut s, feed, ids) = seeded();
    let id = ids[0];
    s.save_fulltext(id, "Koko juttu kertoo kuntavaalit lähestyvän.", false, None).unwrap();
    integrity(&s);
    assert_eq!(found(&s, "kuntavaalit"), vec![id]);
    assert_eq!(found(&s, "content:kuntavaalit"), vec![id]);
    assert!(found(&s, "title:kuntavaalit").is_empty());
    let before = s.fulltext(id).unwrap().unwrap();
    assert_eq!(before.text, "Koko juttu kertoo kuntavaalit lähestyvän.");
    assert!(!before.paywalled);

    let stats = s.ingest(feed, &[entry("1", "Hallitus esittää muutoksia", "Päivitetty kuvaus.", OCT1)]).unwrap();
    assert_eq!(stats.updated, 1);
    integrity(&s);
    assert_eq!(s.fulltext(id).unwrap().unwrap(), before);
    assert_eq!(found(&s, "kuntavaalit"), vec![id]);
    assert_eq!(found(&s, "päivitetty"), vec![id]);
    assert!(found(&s, "päivärahaa").is_empty(), "the old summary left the index");

    // A second fetch replaces the text in the index.
    s.save_fulltext(id, "Uusi versio puhuu eduskunnasta.", true, Some(("pictures/1.jpg", 10))).unwrap();
    integrity(&s);
    assert!(found(&s, "kuntavaalit").is_empty());
    assert_eq!(found(&s, "eduskunnasta"), vec![id]);
    let after = s.fulltext(id).unwrap().unwrap();
    assert!(after.paywalled);
    assert_eq!(after.picture_path.as_deref(), Some("pictures/1.jpg"));
}

#[test]
fn article_text_prefers_fetched() {
    let (s, _, ids) = seeded();
    assert_eq!(s.article_text(ids[0]).unwrap(), "Esitys koskee päivärahaa.");
    s.save_fulltext(ids[0], "Koko artikkeli.", false, None).unwrap();
    assert_eq!(s.article_text(ids[0]).unwrap(), "Koko artikkeli.");
    // An empty fetch does not hide the feed text.
    s.save_fulltext(ids[1], "", true, None).unwrap();
    assert_eq!(s.article_text(ids[1]).unwrap(), "Ilmatieteen laitos varoittaa.");
    integrity(&s);
}

#[test]
fn fts_consistent_after_article_delete_retention_and_feed_removal() {
    let (mut s, feed, ids) = seeded();
    for (i, id) in ids.iter().enumerate() {
        s.save_fulltext(*id, &format!("arkistoteksti numero{i}"), false, None).unwrap();
    }
    integrity(&s);

    // Direct delete.
    s.conn().execute("DELETE FROM articles WHERE id = ?1", [ids[0]]).unwrap();
    integrity(&s);
    assert!(s.fulltext(ids[0]).unwrap().is_none());
    assert_eq!(found(&s, "arkistoteksti").len(), 2);

    // Retention (all fixtures are older than 0 days), keeping a saved one.
    s.set_saved(ids[2], true).unwrap();
    assert_eq!(s.apply_retention(0).unwrap(), 1);
    integrity(&s);
    assert_eq!(found(&s, "arkistoteksti"), vec![ids[2]]);

    // Feed removal cascades through articles to the fetched text.
    let other = s
        .add_feed(&NewFeed { url: "https://en.example/rss".into(), title: "En".into(), ..Default::default() })
        .unwrap();
    s.ingest(other, &[entry("x", "Tram line", "Council voted.", OCT1)]).unwrap();
    let x = *article_ids(&s).last().unwrap();
    s.save_fulltext(x, "arkistoteksti english", false, None).unwrap();
    s.remove_feed(feed).unwrap();
    integrity(&s);
    assert_eq!(found(&s, "arkistoteksti"), vec![x]);
    let n: i64 = s.conn().query_row("SELECT count(*) FROM article_fulltext", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
}

#[test]
fn lowering_limit_evicts_oldest_unsaved_only() {
    let (s, _, ids) = seeded();
    let text = "x".repeat(100);
    for (i, id) in ids.iter().enumerate() {
        s.save_fulltext(*id, &text, false, Some((&format!("pictures/{i}.jpg"), 0))).unwrap();
        set_fetched_at(&s, *id, 1_000 + i as i64);
    }
    s.set_saved(ids[0], true).unwrap();
    assert_eq!(s.fulltext_cache_bytes().unwrap(), 300);

    // Under the limit: nothing happens.
    assert!(s.enforce_fulltext_limit(300).unwrap().is_empty());

    // 300 > 150: the middle (oldest unsaved) goes first, 200 is still over,
    // so the newest goes too. The saved one stays although 100 > 50.
    assert_eq!(s.enforce_fulltext_limit(150).unwrap(), vec!["pictures/1.jpg".to_string(), "pictures/2.jpg".into()]);
    assert_eq!(s.fulltext_cache_bytes().unwrap(), 100);
    assert!(s.fulltext(ids[0]).unwrap().is_some());
    assert!(s.fulltext(ids[1]).unwrap().is_none());
    assert!(s.enforce_fulltext_limit(50).unwrap().is_empty(), "saved text is never evicted");
    assert_eq!(count(&s, "articles"), 3, "the articles stay");
    integrity(&s);

    // Picture bytes count, and eviction stops as soon as the total fits.
    for (i, id) in ids.iter().enumerate().skip(1) {
        s.save_fulltext(*id, &text, false, Some((&format!("pictures/{i}.jpg"), 50))).unwrap();
        set_fetched_at(&s, *id, 2_000 + i as i64);
    }
    assert_eq!(s.fulltext_cache_bytes().unwrap(), 400);
    assert_eq!(s.enforce_fulltext_limit(250).unwrap(), vec!["pictures/1.jpg".to_string()]);
    assert_eq!(s.fulltext_cache_bytes().unwrap(), 250);
    integrity(&s);
}

fn count(s: &Store, table: &str) -> i64 {
    s.conn().query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap()
}

#[test]
fn clear_removes_saved_too() {
    let (s, _, ids) = seeded();
    s.save_fulltext(ids[0], "tallennettu teksti", false, Some(("pictures/a.png", 5))).unwrap();
    s.save_fulltext(ids[1], "toinen teksti", false, None).unwrap();
    s.save_fulltext(ids[2], "kolmas teksti", false, Some(("pictures/c.png", 5))).unwrap();
    s.set_saved(ids[0], true).unwrap();
    let mut paths = s.fulltext_picture_paths().unwrap();
    paths.sort();
    assert_eq!(paths, vec!["pictures/a.png".to_string(), "pictures/c.png".into()]);

    let mut removed = s.clear_fulltext().unwrap();
    removed.sort();
    assert_eq!(removed, vec!["pictures/a.png".to_string(), "pictures/c.png".into()]);
    assert_eq!(s.fulltext_cache_bytes().unwrap(), 0);
    assert!(s.fulltext_picture_paths().unwrap().is_empty());
    assert!(found(&s, "teksti").is_empty());
    assert_eq!(count(&s, "articles"), 3);
    integrity(&s);
}

#[test]
fn save_for_missing_article_is_not_found() {
    let (s, _, _) = seeded();
    assert!(matches!(s.save_fulltext(9_999, "teksti", false, None), Err(Error::NotFound)));
    assert!(s.fulltext(9_999).unwrap().is_none());
    assert_eq!(count(&s, "article_fulltext"), 0);
    integrity(&s);
}

#[test]
fn text_bytes_are_utf8_bytes() {
    let (s, _, ids) = seeded();
    s.save_fulltext(ids[0], "äö", false, Some(("p.jpg", 7))).unwrap();
    assert_eq!(s.fulltext_cache_bytes().unwrap(), 4 + 7);
    integrity(&s);
}

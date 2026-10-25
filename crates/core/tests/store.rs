use juttusihti_core::search::{HIGHLIGHT_END, HIGHLIGHT_START, SearchQuery, matching_ids};
use juttusihti_core::store::{ListFilter, Store};
use juttusihti_core::*;

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

/// 2026-10-01T00:00:00Z
const OCT1: i64 = 1_790_812_800;
const DAY: i64 = 86_400;

fn seeded() -> (Store, FeedId, FeedId) {
    let mut s = Store::open_in_memory().unwrap();
    let folder = s.create_folder("Kotimaa").unwrap();
    let fi = s
        .add_feed(&NewFeed {
            url: "https://fi.example/rss".into(),
            title: "Suomi".into(),
            folder_id: Some(folder),
            ..Default::default()
        })
        .unwrap();
    let en = s
        .add_feed(&NewFeed { url: "https://en.example/rss".into(), title: "English".into(), ..Default::default() })
        .unwrap();
    s.ingest(
        fi,
        &[
            entry(
                "1",
                "Hallitus esittää muutoksia työttömyysturvaan",
                "<p>Esitys koskee <b>ansiosidonnaista</b> päivärahaa.</p>",
                OCT1,
            ),
            entry(
                "2",
                "Sää: Lumisade jatkuu Pohjois-Suomessa",
                "Ilmatieteen laitos varoittaa liukkaista keleistä.",
                OCT1 + DAY,
            ),
            entry("3", "Saa nähdä, mitä tapahtuu", "Uutinen ilman säätä.", OCT1 + 2 * DAY),
        ],
    )
    .unwrap();
    s.ingest(
        en,
        &[
            entry("a", "City council approves tram line", "The council voted on the new tram line on Tuesday.", OCT1),
            entry("b", "Weather: heavy snow in northern Finland", "Snow continues in Lapland.", OCT1 + DAY),
        ],
    )
    .unwrap();
    (s, fi, en)
}

fn titles(s: &Store, q: &SearchQuery) -> Vec<String> {
    s.search(q, Page::first(50)).unwrap().items.into_iter().map(|a| a.title).collect()
}

fn text(q: &str) -> SearchQuery {
    SearchQuery { text: q.into(), ..Default::default() }
}

#[test]
fn ingest_is_idempotent_and_detects_updates() {
    let (mut s, fi, _) = seeded();
    let again = s
        .ingest(
            fi,
            &[entry(
                "1",
                "Hallitus esittää muutoksia työttömyysturvaan",
                "<p>Esitys koskee <b>ansiosidonnaista</b> päivärahaa.</p>",
                OCT1,
            )],
        )
        .unwrap();
    assert_eq!(again, IngestStats { inserted: 0, updated: 0, unchanged: 1 });

    let id = s.search(&text("työttömyysturvaan"), Page::first(5)).unwrap().items[0].id;
    s.set_read(id, true).unwrap();
    s.set_saved(id, true).unwrap();
    let changed = s.ingest(fi, &[entry("1", "Hallitus perui muutokset", "Uusi teksti.", OCT1)]).unwrap();
    assert_eq!(changed.updated, 1);
    let a = s.article(id).unwrap();
    assert_eq!(a.title, "Hallitus perui muutokset");
    assert!(a.is_read && a.is_saved, "user state survives source updates");
    // FTS follows the update.
    assert!(titles(&s, &text("työttömyysturvaan")).is_empty());
    assert_eq!(titles(&s, &text("perui")), vec!["Hallitus perui muutokset"]);
}

#[test]
fn same_url_with_new_guid_is_not_duplicated() {
    let (mut s, fi, _) = seeded();
    let mut dup = entry("1-republished", "Hallitus esittää muutoksia", "x", OCT1);
    dup.url = Some("https://news.example/1?utm_source=twitter".into());
    assert_eq!(s.ingest(fi, &[dup]).unwrap().unchanged, 1);
}

#[test]
fn cross_feed_syndication_is_linked_not_dropped() {
    let (mut s, _, en) = seeded();
    let mut copy = entry("syndicated", "Hallitus esittää muutoksia (STT)", "x", OCT1);
    copy.url = Some("https://news.example/1".into());
    s.ingest(en, &[copy]).unwrap();
    let ids = matching_ids(s.conn(), &text("hallitus"), 10).unwrap();
    assert_eq!(ids.len(), 2);
    assert_eq!(s.duplicates_of(ids[0]).unwrap(), vec![ids[1]]);
}

#[test]
fn finnish_search_keeps_umlauts_distinct() {
    let (s, _, _) = seeded();
    // "sää" (weather) must not match "saa" (gets) and vice versa.
    assert_eq!(titles(&s, &text("sää")), vec!["Sää: Lumisade jatkuu Pohjois-Suomessa"]);
    assert_eq!(titles(&s, &text("saa")), vec!["Saa nähdä, mitä tapahtuu"]);
    // Case-folding works for non-ASCII letters.
    assert_eq!(titles(&s, &text("SÄÄ")).len(), 1);
    // Finnish inflection needs prefix search: "työttömyysturva" alone does not match "työttömyysturvaan".
    assert!(titles(&s, &text("työttömyysturva")).is_empty());
    assert_eq!(titles(&s, &text("työttömyysturva*")).len(), 1);
    // HTML in the original summary is not indexed, only its text.
    assert!(titles(&s, &text("b")).is_empty());
    assert_eq!(titles(&s, &text("ansiosidonnaista")).len(), 1);
}

#[test]
fn boolean_phrase_near_and_column_search() {
    let (s, _, _) = seeded();
    assert_eq!(titles(&s, &text("snow OR lumisade")).len(), 2);
    assert_eq!(titles(&s, &text("council tram")).len(), 1);
    assert_eq!(titles(&s, &text("council -tram")).len(), 0);
    assert_eq!(titles(&s, &text("\"tram line\"")).len(), 1);
    assert_eq!(titles(&s, &text("\"line tram\"")).len(), 0);
    assert_eq!(titles(&s, &text("NEAR(council tuesday, 10)")).len(), 1);
    assert_eq!(titles(&s, &text("NEAR(council tuesday, 1)")).len(), 0);
    assert_eq!(titles(&s, &text("title:snow")).len(), 1);
    assert_eq!(titles(&s, &text("title:lapland")).len(), 0);
    assert_eq!(titles(&s, &text("summary:lapland")).len(), 1);
}

#[test]
fn ranking_prefers_title_matches_and_highlights() {
    let (s, _, _) = seeded();
    let r = s.search(&text("council"), Page::first(10)).unwrap();
    assert_eq!(r.total, 1);
    let snippet = r.items[0].snippet.clone().unwrap();
    assert!(snippet.contains(&format!("{HIGHLIGHT_START}council{HIGHLIGHT_END}")), "{snippet}");
    assert_eq!(r.interpreted.as_deref(), Some("council"));
}

#[test]
fn filters_and_half_open_date_ranges() {
    let (s, fi, en) = seeded();
    let q = SearchQuery { from: Some(OCT1), to: Some(OCT1 + DAY), ..Default::default() };
    // [Oct 1, Oct 2): the Oct 2 00:00 articles are excluded.
    assert_eq!(titles(&s, &q).len(), 2);
    let q = SearchQuery { from: Some(OCT1 + DAY), to: Some(OCT1 + 2 * DAY), ..Default::default() };
    assert_eq!(titles(&s, &q).len(), 2);
    let q = SearchQuery { feed_ids: vec![en], ..Default::default() };
    assert_eq!(titles(&s, &q).len(), 2);
    let folder = s.folders().unwrap()[0].id;
    let q = SearchQuery { folder_ids: vec![folder], ..Default::default() };
    assert_eq!(titles(&s, &q).len(), 3);
    let q = SearchQuery { text: "snow OR lumisade".into(), feed_ids: vec![fi], ..Default::default() };
    assert_eq!(titles(&s, &q), vec!["Sää: Lumisade jatkuu Pohjois-Suomessa"]);
}

#[test]
fn views_pagination_and_read_state() {
    let (s, fi, _) = seeded();
    let (page1, total) = s.list_articles(ListFilter::default(), Page::first(2)).unwrap();
    assert_eq!((page1.len(), total), (2, 5));
    let (page3, _) = s.list_articles(ListFilter::default(), Page::first(2).next().next()).unwrap();
    assert_eq!(page3.len(), 1);
    // Newest first.
    assert!(page1[0].effective_at >= page1[1].effective_at);

    let unread = ListFilter { view: View::Unread, ..Default::default() };
    assert_eq!(s.list_articles(unread, Page::first(10)).unwrap().1, 5);
    assert_eq!(s.mark_all_read(ListFilter { feed_id: Some(fi), ..Default::default() }).unwrap(), 3);
    assert_eq!(s.list_articles(unread, Page::first(10)).unwrap().1, 2);
    let counts = s.unread_counts().unwrap();
    assert_eq!(counts.iter().find(|(f, _)| *f == fi), None);

    let id = page1[0].id;
    s.set_saved(id, true).unwrap();
    let saved = ListFilter { view: View::Saved, ..Default::default() };
    assert_eq!(s.list_articles(saved, Page::first(10)).unwrap().0[0].id, id);
}

#[test]
fn removing_feed_cascades_and_keeps_fts_in_sync() {
    let (s, fi, _) = seeded();
    s.remove_feed(fi).unwrap();
    assert!(titles(&s, &text("sää")).is_empty());
    let n: i64 = s.conn().query_row("SELECT count(*) FROM articles", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 2);
    // FTS5 integrity check passes after cascading deletes.
    s.conn().execute("INSERT INTO articles_fts(articles_fts) VALUES ('integrity-check')", []).unwrap();
}

#[test]
fn retention_keeps_saved_and_researched_articles() {
    let (s, _, _) = seeded();
    let ids = matching_ids(s.conn(), &SearchQuery::default(), 10).unwrap();
    s.set_saved(ids[0], true).unwrap();
    s.annotate(ids[1], None, "tärkeä").unwrap();
    // All fixtures are older than 0 days from now.
    assert_eq!(s.apply_retention(0).unwrap(), 3);
    assert_eq!(matching_ids(s.conn(), &SearchQuery::default(), 10).unwrap().len(), 2);
}

#[test]
fn saved_searches_round_trip_and_validate() {
    let (s, fi, _) = seeded();
    let q = SearchQuery { text: "sää OR snow".into(), feed_ids: vec![fi], ..Default::default() };
    s.save_search("Sää", &q).unwrap();
    let all = s.saved_searches().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].2, q);
    assert!(s.save_search("bad", &text("\"unterminated")).is_err());
}

#[test]
fn invalid_input_is_rejected() {
    let s = Store::open_in_memory().unwrap();
    assert!(s.add_feed(&NewFeed { url: "file:///etc/passwd".into(), ..Default::default() }).is_err());
    assert!(s.add_feed(&NewFeed { url: "  ".into(), ..Default::default() }).is_err());
    assert!(matches!(s.article(42), Err(Error::NotFound)));
}

#[test]
fn ai_labels_are_suggestions_until_user_decides() {
    let (s, _, _) = seeded();
    let ids = matching_ids(s.conn(), &SearchQuery::default(), 10).unwrap();
    let weather = s.create_label("Sää").unwrap();
    assert_eq!(s.create_label("  sää ").unwrap(), weather, "normalized duplicate");
    s.tag_article(ids[0], weather, Origin::Ai, None).unwrap();
    s.tag_article(ids[1], weather, Origin::User, None).unwrap();

    // Suggested labels are not used as filters unless asked.
    let q = SearchQuery { label_ids: vec![weather], ..Default::default() };
    assert_eq!(titles(&s, &q).len(), 1);
    let q = SearchQuery { label_ids: vec![weather], include_suggested: true, ..Default::default() };
    assert_eq!(titles(&s, &q).len(), 2);

    // A user rejection is not overridden by a later AI suggestion.
    s.decide_label(ids[0], weather, Status::Rejected).unwrap();
    s.tag_article(ids[0], weather, Origin::Ai, None).unwrap();
    assert_eq!(s.article_labels(ids[0]).unwrap()[0].status, Status::Rejected);

    // Bulk acceptance only touches pending suggestions.
    s.tag_article(ids[2], weather, Origin::Ai, None).unwrap();
    assert_eq!(s.bulk_decide_label(weather, Status::Confirmed).unwrap(), 1);
    assert_eq!(s.article_labels(ids[0]).unwrap()[0].status, Status::Rejected);
}

#[test]
fn label_merge_and_similarity() {
    let (mut s, _, _) = seeded();
    let ids = matching_ids(s.conn(), &SearchQuery::default(), 10).unwrap();
    let a = s.create_label("Vaali").unwrap();
    let b = s.create_label("Vaalit").unwrap();
    assert_eq!(s.similar_labels("vaalit").unwrap().len(), 2);
    s.tag_article(ids[0], a, Origin::Ai, None).unwrap();
    s.tag_article(ids[0], b, Origin::User, None).unwrap();
    s.tag_article(ids[1], a, Origin::User, None).unwrap();
    s.merge_labels(a, b).unwrap();
    assert_eq!(s.labels().unwrap().len(), 1);
    assert_eq!(s.article_labels(ids[0]).unwrap()[0].status, Status::Confirmed);
    assert_eq!(s.article_labels(ids[1]).unwrap()[0].label.id, b);

    s.tag_article(ids[2], b, Origin::Ai, None).unwrap();
    let empty = s.create_label("Tyhjä").unwrap();
    let counts = s.label_counts().unwrap();
    let find = |id| counts.iter().find(|(l, ..)| l.id == id).map(|(_, c, sg)| (*c, *sg)).unwrap();
    assert_eq!(find(b), (2, 1));
    assert_eq!(find(empty), (0, 0), "unused labels are listed");
}

#[test]
fn entities_resolve_conservatively_and_merge() {
    let (mut s, _, _) = seeded();
    let ids = matching_ids(s.conn(), &SearchQuery::default(), 10).unwrap();
    let helsinki = s.resolve_entity(EntityKind::Place, "Helsinki").unwrap();
    assert_eq!(s.resolve_entity(EntityKind::Place, "HELSINKI").unwrap(), helsinki);
    let org = s.resolve_entity(EntityKind::Organization, "Helsinki").unwrap();
    assert_ne!(org, helsinki, "different kinds stay separate");
    let hki = s.resolve_entity(EntityKind::Place, "Hki").unwrap();
    s.link_entity(ids[0], hki, "event", Some("Hki"), Origin::Ai, None).unwrap();
    s.merge_entities(hki, helsinki).unwrap();
    assert_eq!(s.resolve_entity(EntityKind::Place, "hki").unwrap(), helsinki, "merged name becomes alias");
    let mentions = s.article_entities(ids[0]).unwrap();
    assert_eq!(mentions[0].1.id, helsinki);
    assert_eq!(mentions[0].3, Status::Suggested);
    assert!(s.merge_entities(org, helsinki).is_err());
    // Correcting a wrong merge.
    s.reassign_mention(mentions[0].0, org).unwrap();
    assert_eq!(s.article_entities(ids[0]).unwrap()[0].1.id, org);
}

#[test]
fn relationships_are_typed_symmetric_and_reviewable() {
    let (s, _, _) = seeded();
    let ids = matching_ids(s.conn(), &SearchQuery::default(), 10).unwrap();
    let r1 = s
        .relate(ids[0], ids[1], RelationshipKind::SameEvent, Origin::Ai, Some("both mention snow"), Some(0.7), None)
        .unwrap();
    let r2 = s.relate(ids[1], ids[0], RelationshipKind::SameEvent, Origin::Ai, None, None, None).unwrap();
    assert_eq!(r1, r2, "symmetric relation stored once");
    let f1 = s.relate(ids[0], ids[1], RelationshipKind::FollowUp, Origin::User, None, None, None).unwrap();
    let f2 = s.relate(ids[1], ids[0], RelationshipKind::FollowUp, Origin::User, None, None, None).unwrap();
    assert_ne!(f1, f2, "directed relation keeps direction");
    assert!(s.relate(ids[0], ids[0], RelationshipKind::Related, Origin::User, None, None, None).is_err());
    assert!(s.relate(ids[0], ids[2], RelationshipKind::Related, Origin::Ai, None, Some(1.5), None).is_err());

    let rels = s.relationships(ids[0]).unwrap();
    assert_eq!(rels.len(), 3);
    assert_eq!(rels[0].status, Status::Suggested);
    s.decide_relationship(r1, Status::Rejected).unwrap();
    assert_eq!(s.relationships(ids[0]).unwrap().len(), 2, "rejected links are hidden");
}

#[test]
fn locations_require_coordinate_provenance() {
    let (s, _, _) = seeded();
    let id = matching_ids(s.conn(), &SearchQuery::default(), 1).unwrap()[0];
    let mut loc = research::NewLocation {
        place_name: "Rovaniemi".into(),
        country: Some("FI".into()),
        coordinates: Some((66.5, 25.7)),
        role: LocationRole::Event,
        evidence: Some("Lapissa".into()),
        confidence: Some(0.6),
        coordinate_source: None,
        ambiguous: false,
    };
    assert!(s.add_location(id, &loc, Origin::Ai).is_err(), "coordinates without source");
    loc.coordinate_source = Some("user".into());
    s.add_location(id, &loc, Origin::Ai).unwrap();
    loc.coordinates = Some((95.0, 0.0));
    assert!(s.add_location(id, &loc, Origin::User).is_err());
    loc.coordinates = None;
    loc.ambiguous = true;
    s.add_location(id, &loc, Origin::Ai).unwrap();
}

#[test]
fn sessions_snapshot_articles_and_enforce_lifecycle() {
    let (mut s, _, _) = seeded();
    let ids =
        matching_ids(s.conn(), &SearchQuery { from: Some(OCT1), to: Some(OCT1 + DAY), ..Default::default() }, 100)
            .unwrap();
    let sid = s.create_session(&serde_json::json!({"tasks": ["labels"]}), &ids).unwrap();
    assert_eq!(s.session(sid).unwrap().status, SessionStatus::Draft);
    assert!(s.set_session_status(sid, SessionStatus::Running, None).is_err(), "no run without estimate + approval");
    s.set_session_estimate(sid, &serde_json::json!({"input_tokens": 1200})).unwrap();
    s.set_session_status(sid, SessionStatus::AwaitingApproval, None).unwrap();
    s.set_session_status(sid, SessionStatus::Running, None).unwrap();
    s.set_session_status(sid, SessionStatus::Cancelled, None).unwrap();
    assert!(s.set_session_status(sid, SessionStatus::Running, None).is_err());
    let mut snap = s.session_articles(sid).unwrap();
    snap.sort();
    let mut expected = ids.clone();
    expected.sort();
    assert_eq!(snap, expected);
    // Cancelling never touches source articles.
    assert_eq!(matching_ids(s.conn(), &SearchQuery::default(), 100).unwrap().len(), 5);
}

#[test]
fn wal_allows_concurrent_reader_during_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("j.db");
    let mut s = Store::open(&path).unwrap();
    let feed = s.add_feed(&NewFeed { url: "https://x.example/rss".into(), ..Default::default() }).unwrap();
    s.ingest(feed, &[entry("1", "First", "a", OCT1)]).unwrap();
    let reader = db::open_read_only(&path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    let before: i64 = reader.query_row("SELECT count(*) FROM articles", [], |r| r.get(0)).unwrap();
    s.ingest(feed, &[entry("2", "Second", "b", OCT1)]).unwrap();
    let during: i64 = reader.query_row("SELECT count(*) FROM articles", [], |r| r.get(0)).unwrap();
    assert_eq!((before, during), (1, 1), "reader sees a consistent snapshot");
    reader.execute_batch("COMMIT").unwrap();
    let after: i64 = reader.query_row("SELECT count(*) FROM articles", [], |r| r.get(0)).unwrap();
    assert_eq!(after, 2);
}

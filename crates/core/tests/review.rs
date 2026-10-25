use juttusihti_core::research::NewSuggestion;
use juttusihti_core::review::ReviewKind;
use juttusihti_core::search::{SearchQuery, matching_ids};
use juttusihti_core::store::Store;
use juttusihti_core::*;

fn seeded() -> (Store, Vec<i64>, i64, i64) {
    let mut s = Store::open_in_memory().unwrap();
    let f =
        s.add_feed(&NewFeed { url: "https://f.example/rss".into(), title: "F".into(), ..Default::default() }).unwrap();
    let entries: Vec<NewArticle> = (0..4)
        .map(|i| NewArticle {
            guid: format!("{i}"),
            url: Some(format!("https://f.example/{i}")),
            title: format!("Artikkeli {i} Helsingistä"),
            summary: None,
            content: None,
            author: None,
            published_at: Some(1_790_812_800 + i),
        })
        .collect();
    s.ingest(f, &entries).unwrap();
    let ids = matching_ids(s.conn(), &SearchQuery::default(), 10).unwrap();
    let s1 = s.create_session(&serde_json::json!({}), &ids).unwrap();
    let s2 = s.create_session(&serde_json::json!({}), &ids).unwrap();
    (s, ids, s1, s2)
}

fn suggestion(s: &Store, session: i64, kind: &str) -> i64 {
    s.record_suggestion(&NewSuggestion {
        session_id: Some(session),
        kind: kind.into(),
        payload: serde_json::json!({"summary": "S", "key_points": [], "open_questions": []}),
        provider: "anthropic".into(),
        model: "m".into(),
        prompt_version: "v".into(),
        confidence: None,
        evidence: None,
    })
    .unwrap()
}

#[test]
fn queues_are_scoped_to_their_session() {
    let (s, ids, s1, s2) = seeded();
    let sg1 = suggestion(&s, s1, "extraction");
    let sg2 = suggestion(&s, s2, "extraction");
    let hki = s.resolve_entity(EntityKind::Place, "Helsinki").unwrap();
    s.link_entity(ids[0], hki, "event_location", Some("Helsingistä"), Origin::Ai, Some(sg1)).unwrap();
    s.link_entity(ids[1], hki, "mentioned", None, Origin::Ai, Some(sg2)).unwrap();
    s.relate(ids[0], ids[1], RelationshipKind::SameEvent, Origin::Ai, Some("same"), Some(0.9), Some(sg1)).unwrap();
    s.record_relevance(ids[0], Some(s1), "relevant", Some("on topic"), Origin::Ai, Some(sg1)).unwrap();
    s.record_relevance(ids[1], Some(s1), "clickbait", Some("bait"), Origin::Ai, Some(sg1)).unwrap();

    let c = s.review_counts(s1).unwrap();
    assert_eq!((c.entities, c.relationships, c.relevance, c.has_summary), (1, 1, 2, false));
    assert_eq!(s.review_counts(s2).unwrap().entities, 1);

    let m = s.session_mentions(s1, true, 100).unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].entity_name, "Helsinki");
    assert_eq!(m[0].evidence.as_deref(), Some("Helsingistä"));
    s.decide_mention(m[0].mention_id, Status::Confirmed).unwrap();
    assert!(s.session_mentions(s1, true, 100).unwrap().is_empty());
    assert_eq!(s.session_mentions(s1, false, 100).unwrap().len(), 1, "decided items stay visible in the full list");

    let r = s.session_relationships(s1, true, 100).unwrap();
    assert_eq!(r[0].kind, RelationshipKind::SameEvent);
    s.retype_relationship(r[0].id, RelationshipKind::FollowUp).unwrap();
    let all = s.session_relationships(s1, false, 100).unwrap();
    assert_eq!((all[0].kind, all[0].status), (RelationshipKind::FollowUp, Status::Confirmed));

    let rel = s.session_relevance(s1, true, 100).unwrap();
    assert_eq!(rel[0].class, "relevant", "relevant articles are listed first");
    assert_eq!(s.bulk_decide_session(s1, ReviewKind::Relevance, Status::Confirmed).unwrap(), 2);
    assert_eq!(s.bulk_decide_session(s2, ReviewKind::Entities, Status::Rejected).unwrap(), 1);
}

#[test]
fn hiding_dismissed_articles_is_reversible_and_needs_confirmation() {
    let (s, ids, s1, _) = seeded();
    s.record_relevance(ids[0], Some(s1), "clickbait", None, Origin::Ai, None).unwrap();
    let hide = SearchQuery { hide_dismissed: true, ..Default::default() };
    assert_eq!(matching_ids(s.conn(), &hide, 10).unwrap().len(), 4, "AI suggestion alone hides nothing");
    let row = &s.session_relevance(s1, true, 10).unwrap()[0];
    s.decide_relevance(row.id, Status::Confirmed).unwrap();
    assert_eq!(matching_ids(s.conn(), &hide, 10).unwrap().len(), 3);
    assert_eq!(matching_ids(s.conn(), &SearchQuery::default(), 10).unwrap().len(), 4, "nothing is deleted");
}

#[test]
fn user_relevance_is_not_overwritten_by_ai() {
    let (s, ids, s1, _) = seeded();
    s.record_relevance(ids[0], Some(s1), "relevant", Some("mine"), Origin::User, None).unwrap();
    s.record_relevance(ids[0], Some(s1), "clickbait", Some("ai"), Origin::Ai, None).unwrap();
    let row = &s.session_relevance(s1, false, 10).unwrap()[0];
    assert_eq!((row.class.as_str(), row.status), ("relevant", Status::Confirmed));
    assert!(s.record_relevance(ids[0], Some(s1), "spam", None, Origin::Ai, None).is_err());
}

#[test]
fn summary_and_titles() {
    let (s, ids, s1, s2) = seeded();
    assert!(s.session_summary(s1).unwrap().is_none());
    suggestion(&s, s1, "summary");
    assert!(s.review_counts(s1).unwrap().has_summary);
    assert_eq!(s.session_summary(s1).unwrap().unwrap()["summary"], "S");
    assert!(s.session_summary(s2).unwrap().is_none());
    let titles = s.article_titles(&[ids[0], 999]).unwrap();
    assert_eq!(titles.len(), 1);
}

#[test]
fn locations_carry_provenance_and_user_placement() {
    let (s, ids, s1, s2) = seeded();
    let loc = |name: &str, role| juttusihti_core::research::NewLocation {
        place_name: name.into(),
        country: None,
        coordinates: None,
        role,
        evidence: Some(name.into()),
        confidence: None,
        coordinate_source: None,
        ambiguous: name == "Springfield",
    };
    let hki = s.add_location(ids[0], &loc("Helsinki", LocationRole::Event), Origin::Ai).unwrap();
    let spr = s.add_location(ids[1], &loc("Springfield", LocationRole::Mentioned), Origin::Ai).unwrap();
    let rows = s.locations(Some(s1), 100).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter().all(|r| r.coordinates.is_none() && r.status == Status::Suggested),
        "AI places have no coordinates"
    );
    assert!(rows.iter().find(|r| r.id == spr).unwrap().ambiguous);

    s.set_location_coordinates(hki, Some((60.17, 24.94))).unwrap();
    let h = s.locations(None, 100).unwrap().into_iter().find(|r| r.id == hki).unwrap();
    assert_eq!(h.coordinates, Some((60.17, 24.94)));
    assert_eq!((h.coordinate_source.as_deref(), h.status), (Some("user"), Status::Confirmed));
    assert!(s.set_location_coordinates(hki, Some((91.0, 0.0))).is_err());
    assert!(s.set_location_coordinates(hki, Some((f64::NAN, 0.0))).is_err());

    s.decide_location(spr, Status::Rejected).unwrap();
    assert_eq!(s.locations(None, 100).unwrap().len(), 1, "rejected locations are hidden");
    s.set_location_coordinates(hki, None).unwrap();
    assert_eq!(s.locations(None, 100).unwrap()[0].coordinates, None);
    let _ = s2;
}

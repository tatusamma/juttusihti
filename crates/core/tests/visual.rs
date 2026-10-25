use juttusihti_core::graph::{EdgeKind, GraphQuery, NodeRef};
use juttusihti_core::search::{SearchQuery, matching_ids};
use juttusihti_core::store::Store;
use juttusihti_core::*;

const OCT1: i64 = 1_790_812_800;
const DAY: i64 = 86_400;

fn seeded(n: i64) -> (Store, Vec<i64>) {
    let mut s = Store::open_in_memory().unwrap();
    let f =
        s.add_feed(&NewFeed { url: "https://f.example/rss".into(), title: "F".into(), ..Default::default() }).unwrap();
    let entries: Vec<NewArticle> = (0..n)
        .map(|i| NewArticle {
            guid: i.to_string(),
            url: Some(format!("https://f.example/{i}")),
            title: format!("A{i}"),
            summary: None,
            content: None,
            author: None,
            published_at: if i == n - 1 { None } else { Some(OCT1 + i * DAY) },
        })
        .collect();
    s.ingest(f, &entries).unwrap();
    let mut ids = matching_ids(s.conn(), &SearchQuery::default(), 1000).unwrap();
    ids.sort();
    (s, ids)
}

#[test]
fn graph_respects_hops_status_and_node_cap() {
    let (s, ids) = seeded(6);
    // Chain 0-1-2-3 and a rejected link 3-4.
    s.relate(ids[0], ids[1], RelationshipKind::FollowUp, Origin::User, None, None, None).unwrap();
    s.relate(ids[1], ids[2], RelationshipKind::SameEvent, Origin::Ai, None, Some(0.6), None).unwrap();
    s.relate(ids[2], ids[3], RelationshipKind::Related, Origin::User, None, None, None).unwrap();
    let rejected = s.relate(ids[3], ids[4], RelationshipKind::Duplicate, Origin::Ai, None, None, None).unwrap();
    s.decide_relationship(rejected, Status::Rejected).unwrap();
    let hki = s.resolve_entity(EntityKind::Place, "Helsinki").unwrap();
    s.link_entity(ids[0], hki, "event_location", None, Origin::Ai, None).unwrap();

    let q = |hops, suggested, entities, max| GraphQuery {
        seeds: vec![NodeRef::Article(ids[0])],
        hops,
        include_suggested: suggested,
        include_entities: entities,
        kinds: vec![],
        max_nodes: max,
    };
    let g = s.graph(&q(1, true, true, 100)).unwrap();
    assert_eq!(g.nodes.len(), 3, "seed, its follow-up and the entity: {:?}", g.nodes);
    assert!(g.edges.iter().any(|e| e.kind == EdgeKind::Mentions && e.status == Status::Suggested));

    let g = s.graph(&q(5, true, false, 100)).unwrap();
    assert_eq!(g.nodes.len(), 4, "rejected link to article 4 is never followed");
    let g = s.graph(&q(5, false, false, 100)).unwrap();
    assert_eq!(g.nodes.len(), 2, "suggested link 1-2 hidden when only confirmed are shown");

    let g = s.graph(&q(5, true, true, 2)).unwrap();
    assert_eq!(g.nodes.len(), 2);
    assert!(g.truncated);

    let g = s.graph(&GraphQuery { kinds: vec![RelationshipKind::SameEvent], ..q(5, true, false, 100) }).unwrap();
    assert_eq!(g.nodes.len(), 1, "follow-up link filtered out by kind");

    // Entity seeds and search.
    assert_eq!(s.find_entities("hels", 10).unwrap()[0].id, hki);
    let g = s.graph(&GraphQuery { seeds: vec![NodeRef::Entity(hki)], hops: 1, ..q(1, true, true, 100) }).unwrap();
    assert_eq!(g.nodes.len(), 2);
}

#[test]
fn timeline_clusters_and_keeps_dates_honest() {
    let (s, ids) = seeded(5);
    s.relate(ids[0], ids[2], RelationshipKind::FollowUp, Origin::User, None, None, None).unwrap();
    s.relate(ids[2], ids[3], RelationshipKind::Related, Origin::Ai, None, None, None).unwrap();
    let dated = s.create_event("Vaalit", None, Some(OCT1 + DAY), None, Origin::User).unwrap();
    let undated = s.create_event("Neuvottelut", None, None, None, Origin::Ai).unwrap();
    let outside = s.create_event("Old", None, Some(OCT1 - 100 * DAY), None, Origin::User).unwrap();
    for e in [dated, undated, outside] {
        s.link_event(ids[0], e, Origin::User, None).unwrap();
    }
    let q = SearchQuery { from: Some(OCT1 - DAY), to: Some(OCT1 + 365 * DAY), ..Default::default() };
    let t = s.timeline(&q, 100).unwrap();
    assert_eq!(t.articles.len(), 5);
    assert!(t.articles.windows(2).all(|w| w[0].at <= w[1].at), "oldest first");
    let cluster = |id| t.articles.iter().find(|a| a.id == id).unwrap().cluster;
    assert_eq!(cluster(ids[0]), Some(ids[0]));
    assert_eq!(cluster(ids[3]), Some(ids[0]), "transitive story cluster");
    assert_eq!(cluster(ids[1]), None);
    let undated_article = t.articles.iter().find(|a| a.id == ids[4]).unwrap();
    assert!(!undated_article.dated, "fetch-time fallback is flagged");
    assert_eq!(t.events.iter().map(|e| e.id).collect::<Vec<_>>(), vec![dated]);
    assert_eq!(t.undated_events.len(), 2, "undated and out-of-range events are listed, not placed");

    let t = s.timeline(&q, 3).unwrap();
    assert!(t.truncated);
    assert_eq!(t.articles.len(), 3);
}

#[test]
fn layout_handles_the_node_cap_quickly() {
    let n = 150;
    let edges: Vec<(usize, usize)> = (0..n).flat_map(|i| [(i, (i + 1) % n), (i, (i * 7 + 3) % n)]).collect();
    let started = std::time::Instant::now();
    let pos = juttusihti_core::graph::layout(n, &edges, 300);
    assert!(started.elapsed() < std::time::Duration::from_secs(5), "layout took {:?}", started.elapsed());
    assert_eq!(pos.len(), n);
    assert!(pos.iter().all(|(x, y)| x.is_finite() && y.is_finite()));
    // Nodes are spread out, not collapsed onto one point.
    let distinct: std::collections::HashSet<(i32, i32)> =
        pos.iter().map(|(x, y)| ((x * 100.0) as i32, (y * 100.0) as i32)).collect();
    assert!(distinct.len() > n * 9 / 10);
}

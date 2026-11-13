use juttusihti_core::search::SearchQuery;
use juttusihti_core::store::{ListFilter, Store};
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

// ---- organising API ----------------------------------------------------

use juttusihti_core::categories::*;

fn names(nodes: &[CategoryNode]) -> Vec<String> {
    nodes.iter().map(|n| n.label.name.clone()).collect()
}

fn term_id(s: &Store, name: &str) -> TermId {
    s.conn().query_row("SELECT id FROM feed_terms WHERE name = ?1", [name], |r| r.get(0)).unwrap()
}

#[test]
fn tree_orders_groups_and_categories_by_position() {
    let mut s = Store::open_in_memory().unwrap();
    let g1 = s.create_group("Zeta").unwrap();
    let g2 = s.create_group("Alpha").unwrap();
    let b = s.create_label("b").unwrap();
    let a = s.create_label("a").unwrap();
    s.move_category(b, Some(g1), 0).unwrap();
    s.move_category(a, Some(g1), 1).unwrap();
    let t = s.category_tree().unwrap();
    assert_eq!(t.groups.iter().map(|g| g.group.id).collect::<Vec<_>>(), vec![g1, g2]);
    assert_eq!(names(&t.groups[0].categories), vec!["b", "a"]);
    s.move_group(g2, 0).unwrap();
    let t = s.category_tree().unwrap();
    assert_eq!(t.groups[0].group.id, g2);
    assert!(t.ungrouped.is_empty());
}

#[test]
fn group_names_are_unique_ignoring_case_and_spaces() {
    let s = Store::open_in_memory().unwrap();
    s.create_group("Tech").unwrap();
    assert!(matches!(s.create_group(" tech "), Err(Error::Invalid(_))));
    assert!(matches!(s.create_group("  "), Err(Error::Invalid(_))));
    let g = s.create_group("Other").unwrap();
    assert!(matches!(s.rename_group(g, "TECH"), Err(Error::Invalid(_))));
    s.rename_group(g, "Other 2").unwrap();
}

#[test]
fn deleting_group_ungroups_categories_and_drops_favourite() {
    let mut s = Store::open_in_memory().unwrap();
    let g = s.create_group("G").unwrap();
    let l = s.create_label("L").unwrap();
    s.move_category(l, Some(g), 0).unwrap();
    s.add_favourite(Favourite::Group(g)).unwrap();
    s.delete_group(g).unwrap();
    let t = s.category_tree().unwrap();
    assert!(t.groups.is_empty());
    assert_eq!(names(&t.ungrouped), vec!["L"]);
    assert!(t.favourites.is_empty());
}

#[test]
fn move_category_between_groups_renumbers_both() {
    let mut s = Store::open_in_memory().unwrap();
    let g1 = s.create_group("G1").unwrap();
    let g2 = s.create_group("G2").unwrap();
    let ids: Vec<_> = ["a", "b", "c", "d"].iter().map(|n| s.create_label(n).unwrap()).collect();
    for (i, l) in ids.iter().take(3).enumerate() {
        s.move_category(*l, Some(g1), i).unwrap();
    }
    s.move_category(ids[3], Some(g2), 0).unwrap();
    s.move_category(ids[1], Some(g2), 0).unwrap();
    let t = s.category_tree().unwrap();
    assert_eq!(names(&t.groups[0].categories), vec!["a", "c"]);
    assert_eq!(names(&t.groups[1].categories), vec!["b", "d"]);
    let pos = |l: LabelId| -> i64 {
        s.conn().query_row("SELECT position FROM labels WHERE id = ?1", [l], |r| r.get(0)).unwrap()
    };
    assert_eq!((pos(ids[0]), pos(ids[2])), (0, 1));
    assert_eq!((pos(ids[1]), pos(ids[3])), (0, 1));
    s.move_category(ids[1], None, 99).unwrap();
    assert_eq!(names(&s.category_tree().unwrap().ungrouped), vec!["b"]);
}

#[test]
fn mapping_and_ignoring_terms() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["AI", "Sports", "Misc"])]).unwrap();
    let l = s.create_label("Tech").unwrap();
    assert_eq!(s.unsorted_terms().unwrap().len(), 3);
    let ai = term_id(&s, "AI");
    let sp = term_id(&s, "Sports");
    s.map_term(ai, Some(l)).unwrap();
    s.set_term_ignored(sp, true).unwrap();
    assert_eq!(s.unsorted_terms().unwrap().len(), 1);
    assert_eq!(s.category_terms(l).unwrap()[0].feed_title, "https://a.example/rss");
    assert_eq!(s.category_terms(l).unwrap().len(), 1);
    assert_eq!(s.ignored_terms().unwrap().len(), 1);
    assert_eq!(s.category_tree().unwrap().ungrouped[0].term_count, 1);
    // mapping clears ignored; ignoring clears mapping
    s.map_term(sp, Some(l)).unwrap();
    assert!(s.ignored_terms().unwrap().is_empty());
    assert_eq!(s.category_terms(l).unwrap().len(), 2);
    s.set_term_ignored(ai, true).unwrap();
    assert_eq!(s.category_terms(l).unwrap().len(), 1);
    s.set_term_ignored(ai, false).unwrap();
    assert_eq!(s.unsorted_terms().unwrap().len(), 2);
    s.map_term(sp, None).unwrap();
    assert_eq!(s.category_terms(l).unwrap().len(), 0);
    assert!(matches!(s.map_term(9999, None), Err(Error::NotFound)));
}

#[test]
fn favourites_keep_their_order() {
    let mut s = Store::open_in_memory().unwrap();
    let g = s.create_group("G").unwrap();
    let l = s.create_label("L").unwrap();
    s.add_favourite(Favourite::Category(l)).unwrap();
    s.add_favourite(Favourite::Group(g)).unwrap();
    s.add_favourite(Favourite::Category(l)).unwrap();
    assert_eq!(s.category_tree().unwrap().favourites, vec![Favourite::Category(l), Favourite::Group(g)]);
    s.move_favourite(Favourite::Group(g), 0).unwrap();
    assert_eq!(s.category_tree().unwrap().favourites, vec![Favourite::Group(g), Favourite::Category(l)]);
    s.remove_favourite(Favourite::Group(g)).unwrap();
    assert_eq!(s.category_tree().unwrap().favourites, vec![Favourite::Category(l)]);
}

#[test]
fn counts_are_distinct_and_follow_view_and_feed() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    let f2 = feed(&mut s, "https://b.example/rss");
    s.ingest(f, &[entry("1", &["AI"])]).unwrap();
    let g = s.create_group("G").unwrap();
    let a = s.create_label("A").unwrap();
    let b = s.create_label("B").unwrap();
    s.move_category(a, Some(g), 0).unwrap();
    s.move_category(b, Some(g), 1).unwrap();
    let art = article_id(&s, "1");
    s.tag_article(art, a, Origin::User, None).unwrap();
    s.map_term(term_id(&s, "AI"), Some(b)).unwrap();
    let c = s.category_counts(ListFilter::default()).unwrap();
    assert_eq!(c.groups[&g], 1);
    assert_eq!((c.categories[&a], c.categories[&b]), (1, 1));
    s.set_read(art, true).unwrap();
    let unread = ListFilter { view: View::Unread, ..Default::default() };
    let c = s.category_counts(unread).unwrap();
    assert_eq!(c.groups.get(&g).copied().unwrap_or(0), 0);
    let other = ListFilter { feed_id: Some(f2), ..Default::default() };
    assert!(s.category_counts(other).unwrap().groups.is_empty());
    let own = ListFilter { feed_id: Some(f), ..Default::default() };
    assert_eq!(s.category_counts(own).unwrap().groups[&g], 1);
}

#[test]
fn counts_exclude_rejected_mapped_terms() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["AI"]), entry("2", &["AI"])]).unwrap();
    let g = s.create_group("G").unwrap();
    let l = s.create_label("L").unwrap();
    s.move_category(l, Some(g), 0).unwrap();
    s.map_term(term_id(&s, "AI"), Some(l)).unwrap();
    let c = s.category_counts(ListFilter::default()).unwrap();
    assert_eq!((c.categories[&l], c.groups[&g]), (2, 2));
    s.untag_article(article_id(&s, "1"), l).unwrap();
    let c = s.category_counts(ListFilter::default()).unwrap();
    assert_eq!((c.categories[&l], c.groups[&g]), (1, 1));
    s.untag_article(article_id(&s, "2"), l).unwrap();
    let c = s.category_counts(ListFilter::default()).unwrap();
    assert!(!c.categories.contains_key(&l) && !c.groups.contains_key(&g));
}

#[test]
fn counts_follow_folder_and_saved_view() {
    let mut s = Store::open_in_memory().unwrap();
    let folder = s.create_folder("F").unwrap();
    let other = s.create_folder("Other").unwrap();
    let f1 = s
        .add_feed(&NewFeed {
            url: "https://a.example/rss".into(),
            title: "a".into(),
            folder_id: Some(folder),
            ..Default::default()
        })
        .unwrap();
    let f2 = feed(&mut s, "https://b.example/rss");
    s.ingest(f1, &[entry("1", &["AI"])]).unwrap();
    s.ingest(f2, &[entry("2", &["AI"])]).unwrap();
    let g = s.create_group("G").unwrap();
    let l = s.create_label("L").unwrap();
    s.move_category(l, Some(g), 0).unwrap();
    s.conn().execute("UPDATE feed_terms SET label_id = ?1", [l]).unwrap();
    let all = s.category_counts(ListFilter::default()).unwrap();
    assert_eq!((all.categories[&l], all.groups[&g]), (2, 2));
    let in_folder = ListFilter { folder_id: Some(folder), ..Default::default() };
    let c = s.category_counts(in_folder).unwrap();
    assert_eq!((c.categories[&l], c.groups[&g]), (1, 1));
    let empty = ListFilter { folder_id: Some(other), ..Default::default() };
    assert!(s.category_counts(empty).unwrap().groups.is_empty());
    let saved = ListFilter { view: View::Saved, ..Default::default() };
    assert!(s.category_counts(saved).unwrap().groups.is_empty());
    s.set_saved(article_id(&s, "2"), true).unwrap();
    let c = s.category_counts(saved).unwrap();
    assert_eq!((c.categories[&l], c.groups[&g]), (1, 1));
}

mod draft {
    use super::*;
    use juttusihti_core::category_draft::*;
    use juttusihti_core::pricing::Provider;

    fn term_ids(s: &Store) -> Vec<TermId> {
        s.unsorted_terms().unwrap().iter().map(|t| t.id).collect()
    }

    fn term_named(s: &Store, name: &str) -> TermId {
        s.unsorted_terms().unwrap().iter().find(|t| t.name == name).unwrap().id
    }

    fn setup(names: &[&str]) -> Store {
        let mut s = Store::open_in_memory().unwrap();
        let f = feed(&mut s, "https://a.example/rss");
        s.ingest(f, &[entry("1", names)]).unwrap();
        s
    }

    fn group_of(s: &Store, l: LabelId) -> Option<GroupId> {
        s.conn().query_row("SELECT group_id FROM labels WHERE id = ?1", [l], |r| r.get(0)).unwrap()
    }

    fn label_of(s: &Store, t: TermId) -> Option<LabelId> {
        s.conn().query_row("SELECT label_id FROM feed_terms WHERE id = ?1", [t], |r| r.get(0)).unwrap()
    }

    fn ng(temp: u32, name: &str) -> NewGroupDraft {
        NewGroupDraft { temp, name: name.into(), by_user: false, ticked: true }
    }
    fn nc(temp: u32, name: &str, group: GroupRef) -> NewCategoryDraft {
        NewCategoryDraft { temp, name: name.into(), group, ticked: true }
    }
    fn mp(term: TermId, category: CategoryRef) -> Mapping {
        Mapping { term, category, ticked: true }
    }

    #[test]
    fn plan_input_contains_only_unsorted_terms_and_flags_more() {
        let s = setup(&["a", "b", "c", "d", "e", "f", "g"]);
        let ids = term_ids(&s);
        let l = s.create_label("L").unwrap();
        s.map_term(ids[0], Some(l)).unwrap();
        s.set_term_ignored(ids[1], true).unwrap();
        let g = s.create_group("G").unwrap();
        let input = s.category_plan_input(3).unwrap();
        assert_eq!(input.terms.len(), 3);
        assert_eq!(input.more_terms, 2);
        assert!(input.terms.iter().all(|t| t.id != ids[0] && t.id != ids[1]));
        assert_eq!(input.groups.iter().map(|x| x.id).collect::<Vec<_>>(), vec![g]);
        assert_eq!(input.categories.len(), 1);
    }

    #[test]
    fn draft_round_trips_and_survives_reopen() {
        let s = setup(&["a"]);
        let t = term_ids(&s)[0];
        let mut d = CategoryDraft {
            new_groups: vec![ng(1, "Politiikka")],
            new_categories: vec![nc(1, "Vaalit", GroupRef::New(1))],
            mappings: vec![mp(t, CategoryRef::New(1))],
            vague: vec![t],
            ..Default::default()
        };
        let id = s.save_category_draft(&d, Provider::Anthropic, "m").unwrap();
        assert_eq!(s.pending_category_draft().unwrap(), Some((id, d.clone())));
        d.new_groups[0].ticked = false;
        s.update_category_draft(id, &d).unwrap();
        assert_eq!(s.pending_category_draft().unwrap(), Some((id, d)));
    }

    #[test]
    fn apply_creates_groups_categories_and_mappings() {
        let mut s = setup(&["a", "b"]);
        let (a, b) = (term_named(&s, "a"), term_named(&s, "b"));
        let loose = s.create_label("Loose").unwrap();
        let existing = s.create_group("Old").unwrap();
        let d = CategoryDraft {
            new_groups: vec![ng(1, "Politiikka")],
            new_categories: vec![nc(1, "Vaalit", GroupRef::New(1)), nc(2, "Muu", GroupRef::Existing(existing))],
            placements: vec![Placement {
                label: loose,
                rename: Some("Vapaa".into()),
                group: GroupRef::New(1),
                ticked: true,
            }],
            mappings: vec![mp(a, CategoryRef::New(1)), mp(b, CategoryRef::Existing(loose))],
            vague: vec![],
        };
        let id = s.save_category_draft(&d, Provider::Openai, "m").unwrap();
        let r = s.apply_category_draft(id, &d).unwrap();
        assert_eq!((r.applied, r.skipped), (6, 0));
        let tree = s.category_tree().unwrap();
        let pol = tree.groups.iter().find(|g| g.group.name == "Politiikka").unwrap();
        let names: Vec<_> = pol.categories.iter().map(|c| c.label.name.as_str()).collect();
        assert_eq!(names, vec!["Vaalit", "Vapaa"]);
        assert_eq!(label_of(&s, a), Some(pol.categories[0].label.id));
        assert_eq!(label_of(&s, b), Some(loose));
        assert_eq!(tree.groups.last().unwrap().group.name, "Politiikka");
        assert!(s.pending_category_draft().unwrap().is_none());
        let status: String =
            s.conn().query_row("SELECT status FROM ai_suggestions WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(status, "confirmed");
    }

    #[test]
    fn unticked_group_drops_its_contents() {
        let mut s = setup(&["a"]);
        let a = term_ids(&s)[0];
        let mut g = ng(1, "Politiikka");
        g.ticked = false;
        let d = CategoryDraft {
            new_groups: vec![g],
            new_categories: vec![nc(1, "Vaalit", GroupRef::New(1))],
            mappings: vec![mp(a, CategoryRef::New(1))],
            ..Default::default()
        };
        let id = s.save_category_draft(&d, Provider::Gemini, "m").unwrap();
        let r = s.apply_category_draft(id, &d).unwrap();
        assert_eq!((r.applied, r.skipped), (0, 0));
        assert_eq!(count(&s, "category_groups"), 0);
        assert_eq!(count(&s, "labels"), 0);
        assert_eq!(label_of(&s, a), None);
    }

    #[test]
    fn apply_skips_stale_items() {
        let mut s = setup(&["a", "b"]);
        let (a, b) = (term_named(&s, "a"), term_named(&s, "b"));
        let gone = s.create_group("Gone").unwrap();
        let keep = s.create_group("Keep").unwrap();
        let p1 = s.create_label("P1").unwrap();
        let p2 = s.create_label("P2").unwrap();
        let d = CategoryDraft {
            placements: vec![
                Placement { label: p1, rename: None, group: GroupRef::Existing(gone), ticked: true },
                Placement { label: p2, rename: None, group: GroupRef::Existing(keep), ticked: true },
            ],
            mappings: vec![mp(a, CategoryRef::Existing(p1)), mp(b, CategoryRef::Existing(p1))],
            ..Default::default()
        };
        let id = s.save_category_draft(&d, Provider::Anthropic, "m").unwrap();
        s.map_term(a, Some(p2)).unwrap(); // mapped by hand
        s.delete_group(gone).unwrap(); // deleted target group
        s.move_category(p2, Some(keep), 0).unwrap(); // grouped by hand
        let r = s.apply_category_draft(id, &d).unwrap();
        // skipped: placement p1 (group gone), placement p2 (already grouped), mapping a (already mapped)
        assert_eq!(r.skipped, 3);
        // applied: mapping b
        assert_eq!(r.applied, 1);
        assert_eq!(label_of(&s, b), Some(p1));
        assert_eq!(label_of(&s, a), Some(p2));
    }

    #[test]
    fn apply_refuses_a_draft_that_is_not_pending() {
        let mut s = setup(&["a", "b"]);
        let (a, b) = (term_named(&s, "a"), term_named(&s, "b"));
        let d = CategoryDraft {
            new_groups: vec![ng(1, "G")],
            new_categories: vec![nc(1, "C", GroupRef::New(1))],
            mappings: vec![mp(a, CategoryRef::New(1))],
            ..Default::default()
        };
        // Applied once: a second Approve changes nothing.
        let id = s.save_category_draft(&d, Provider::Openai, "m").unwrap();
        s.apply_category_draft(id, &d).unwrap();
        let again = CategoryDraft { mappings: vec![mp(b, CategoryRef::New(1))], ..d.clone() };
        assert!(matches!(s.apply_category_draft(id, &again), Err(juttusihti_core::Error::NotFound)));
        assert_eq!((count(&s, "category_groups"), count(&s, "labels")), (1, 1));
        assert_eq!(label_of(&s, b), None);
        // Discarded: Approve afterwards changes nothing either.
        let id = s.save_category_draft(&again, Provider::Openai, "m").unwrap();
        s.discard_category_draft(id).unwrap();
        assert!(matches!(s.apply_category_draft(id, &again), Err(juttusihti_core::Error::NotFound)));
        assert_eq!(label_of(&s, b), None);
    }

    #[test]
    fn duplicate_new_group_name_reuses_existing_group() {
        let mut s = setup(&["a"]);
        let a = term_ids(&s)[0];
        let g = s.create_group("Politiikka").unwrap();
        let d = CategoryDraft {
            new_groups: vec![ng(1, "  politiikka ")],
            new_categories: vec![nc(1, "Vaalit", GroupRef::New(1))],
            mappings: vec![mp(a, CategoryRef::New(1))],
            ..Default::default()
        };
        let id = s.save_category_draft(&d, Provider::Anthropic, "m").unwrap();
        let r = s.apply_category_draft(id, &d).unwrap();
        assert_eq!(r.skipped, 0);
        assert_eq!(count(&s, "category_groups"), 1);
        let l = label_of(&s, a).unwrap();
        assert_eq!(group_of(&s, l), Some(g));
    }

    #[test]
    fn discard_leaves_everything_unchanged() {
        let s = setup(&["a"]);
        let a = term_ids(&s)[0];
        let d = CategoryDraft {
            new_groups: vec![ng(1, "G")],
            new_categories: vec![nc(1, "C", GroupRef::New(1))],
            mappings: vec![mp(a, CategoryRef::New(1))],
            ..Default::default()
        };
        let id = s.save_category_draft(&d, Provider::Anthropic, "m").unwrap();
        s.discard_category_draft(id).unwrap();
        assert!(s.pending_category_draft().unwrap().is_none());
        assert_eq!((count(&s, "category_groups"), count(&s, "labels")), (0, 0));
        assert_eq!(label_of(&s, a), None);
    }
}

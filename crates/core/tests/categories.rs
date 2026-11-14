use juttusihti_core::categories::*;
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

/// A feed whose title is `title`.
fn titled(s: &mut Store, title: &str) -> FeedId {
    s.add_feed(&NewFeed { url: format!("https://{title}/rss"), title: title.into(), ..Default::default() }).unwrap()
}

/// Ingests like a refresh, then undoes the automatic filing of the terms it added, for tests
/// that start from terms still waiting in Unsorted.
fn ingest_unsorted(s: &mut Store, feed: FeedId, entries: &[NewArticle]) -> Result<IngestStats> {
    let max = |s: &Store, table: &str| -> i64 {
        s.conn().query_row(&format!("SELECT COALESCE(MAX(id), 0) FROM {table}"), [], |r| r.get(0)).unwrap()
    };
    let (terms, categories) = (max(s, "feed_terms"), max(s, "categories"));
    let stats = s.ingest(feed, entries)?;
    s.conn().execute("UPDATE feed_terms SET category_id = NULL WHERE id > ?1", [terms]).unwrap();
    s.conn().execute("DELETE FROM categories WHERE id > ?1", [categories]).unwrap();
    Ok(stats)
}

fn count(s: &Store, table: &str) -> i64 {
    s.conn().query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap()
}

fn page() -> Page {
    Page { limit: 50, offset: 0 }
}

#[test]
fn ingest_links_terms_idempotently() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    let e = entry("1", &["AI", "Politiikka"]);
    ingest_unsorted(&mut s, f, std::slice::from_ref(&e)).unwrap();
    ingest_unsorted(&mut s, f, std::slice::from_ref(&e)).unwrap();
    assert_eq!(count(&s, "feed_terms"), 2);
    assert_eq!(count(&s, "article_terms"), 2);
}

#[test]
fn same_word_in_two_feeds_is_two_terms() {
    let mut s = Store::open_in_memory().unwrap();
    let f1 = feed(&mut s, "https://a.example/rss");
    let f2 = feed(&mut s, "https://b.example/rss");
    ingest_unsorted(&mut s, f1, &[entry("1", &["AI"])]).unwrap();
    ingest_unsorted(&mut s, f2, &[entry("2", &["AI"])]).unwrap();
    assert_eq!(count(&s, "feed_terms"), 2);
}

#[test]
fn unchanged_article_gains_terms_on_refresh() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(&mut s, f, &[entry("1", &[])]).unwrap();
    assert_eq!(count(&s, "article_terms"), 0);
    let st = ingest_unsorted(&mut s, f, &[entry("1", &["AI"])]).unwrap();
    assert_eq!(st.unchanged, 1);
    assert_eq!(count(&s, "article_terms"), 1);
}

/// The category a term is filed under, if any.
fn filed_under(s: &Store, feed: FeedId, name: &str) -> Option<CategoryId> {
    s.conn()
        .query_row("SELECT category_id FROM feed_terms WHERE feed_id = ?1 AND name = ?2", (feed, name), |r| r.get(0))
        .unwrap()
}

#[test]
fn new_terms_are_filed_under_a_category_of_the_same_name() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["Urheilu", "Jääkiekko"])]).unwrap();
    let tree = s.category_tree().unwrap();
    assert_eq!(names(&tree.ungrouped), ["Urheilu", "Jääkiekko"]);
    assert!(s.unsorted_terms().unwrap().is_empty());
    let urheilu = filed_under(&s, f, "Urheilu").unwrap();
    assert_eq!(in_category(&s, urheilu), ["1"]);
}

#[test]
fn the_same_publisher_category_from_two_feeds_shares_one_category() {
    let mut s = Store::open_in_memory().unwrap();
    let (yle, is) = (feed(&mut s, "https://yle.example/rss"), feed(&mut s, "https://is.example/rss"));
    s.ingest(yle, &[entry("1", &["Kotimaa"])]).unwrap();
    s.ingest(is, &[entry("2", &[" kotimaa "])]).unwrap();
    assert_eq!(count(&s, "categories"), 1);
    assert_eq!(filed_under(&s, yle, "Kotimaa"), filed_under(&s, is, " kotimaa "));
}

#[test]
fn a_publisher_category_joins_an_existing_category_and_keeps_its_group() {
    let mut s = Store::open_in_memory().unwrap();
    let sports = s.create_group("Sports").unwrap();
    let urheilu = s.create_category("Urheilu").unwrap();
    s.move_category(urheilu, Some(sports), 0).unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["URHEILU"])]).unwrap();
    assert_eq!(count(&s, "categories"), 1);
    assert_eq!(filed_under(&s, f, "URHEILU"), Some(urheilu));
    assert_eq!(names(&s.category_tree().unwrap().groups[0].categories), ["Urheilu"]);
}

#[test]
fn a_term_moved_back_to_unsorted_or_ignored_stays_so_on_refresh() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["Tilaajille", "Mielipide"])]).unwrap();
    s.set_term_ignored(term_id(&s, "Tilaajille"), true).unwrap();
    s.map_term(term_id(&s, "Mielipide"), None).unwrap();
    s.ingest(f, &[entry("1", &["Tilaajille", "Mielipide"]), entry("2", &["Tilaajille", "Mielipide"])]).unwrap();
    assert_eq!(filed_under(&s, f, "Tilaajille"), None);
    assert_eq!(s.ignored_terms().unwrap().len(), 1);
    let unsorted: Vec<String> = s.unsorted_terms().unwrap().into_iter().map(|t| t.name).collect();
    assert_eq!(unsorted, ["Mielipide"]);
}

#[test]
fn a_term_whose_category_was_deleted_stays_unsorted_on_refresh() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["Viihde"])]).unwrap();
    s.delete_category(filed_under(&s, f, "Viihde").unwrap()).unwrap();
    s.ingest(f, &[entry("2", &["Viihde"])]).unwrap();
    assert_eq!(count(&s, "categories"), 0);
    assert_eq!(s.unsorted_terms().unwrap().len(), 1);
}

#[test]
fn a_term_without_letters_or_digits_stays_unsorted() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    s.ingest(f, &[entry("1", &["—", "Talous"])]).unwrap();
    assert_eq!(names(&s.category_tree().unwrap().ungrouped), ["Talous"]);
    assert_eq!(filed_under(&s, f, "—"), None);
}

#[test]
fn removing_feed_removes_its_terms() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(&mut s, f, &[entry("1", &["AI"])]).unwrap();
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
    ingest_unsorted(&mut s, f, &[entry("1", &["AI"]), entry("2", &[" ai "])]).unwrap();
    assert_eq!(count(&s, "feed_terms"), 1);
    assert_eq!(count(&s, "article_terms"), 2);
}

#[test]
fn same_name_different_schemes_are_two_terms() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(&mut s, f, &[entry_scheme("1", "AI", ""), entry_scheme("2", "AI", "urn:x")]).unwrap();
    assert_eq!(count(&s, "feed_terms"), 2);
}

fn map_term(s: &Store, name: &str, c: CategoryId) {
    s.conn().execute("UPDATE feed_terms SET category_id = ?1 WHERE name = ?2", rusqlite::params![c, name]).unwrap();
}

fn article_id(s: &Store, guid: &str) -> ArticleId {
    s.conn().query_row("SELECT id FROM articles WHERE guid = ?1", [guid], |r| r.get(0)).unwrap()
}

fn matching(s: &Store, q: &SearchQuery) -> Vec<String> {
    let r = s.search(q, page()).unwrap();
    let mut guids: Vec<String> = r.items.iter().map(|i| s.article(i.id).unwrap().guid).collect();
    guids.sort();
    guids
}

fn in_category(s: &Store, c: CategoryId) -> Vec<String> {
    matching(s, &SearchQuery { category_ids: vec![c], ..Default::default() })
}

fn by_category(s: &Store, c: CategoryId) -> u64 {
    let q = SearchQuery { category_ids: vec![c], ..Default::default() };
    s.search(&q, page()).unwrap().total
}

fn set_ai(s: &Store, article: ArticleId, c: CategoryId) {
    s.conn()
        .execute(
            "INSERT INTO article_categories(article_id, ai_category_id, updated_at) VALUES (?1, ?2, 0)
             ON CONFLICT(article_id) DO UPDATE SET ai_category_id = excluded.ai_category_id",
            [article, c],
        )
        .unwrap();
}

fn term_id(s: &Store, name: &str) -> TermId {
    s.conn().query_row("SELECT id FROM feed_terms WHERE name = ?1", [name], |r| r.get(0)).unwrap()
}

fn names(nodes: &[CategoryNode]) -> Vec<String> {
    nodes.iter().map(|n| n.category.name.clone()).collect()
}

#[test]
fn labels_table_is_untouched_by_migration() {
    let s = Store::open_in_memory().unwrap();
    let cols = |table: &str| -> Vec<String> {
        let mut st = s.conn().prepare(&format!("SELECT name FROM pragma_table_info('{table}') ORDER BY cid")).unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    };
    assert_eq!(cols("labels"), ["id", "name", "normalized", "color", "created_at"]);
    assert_eq!(cols("article_labels"), ["article_id", "label_id", "origin", "status", "suggestion_id", "created_at"]);
    assert_eq!(cols("categories"), ["id", "name", "normalized", "group_id", "position", "created_at"]);
    assert_eq!(cols("article_categories"), ["article_id", "user_category_id", "ai_category_id", "updated_at"]);
    assert!(cols("feed_terms").contains(&"category_id".to_string()));
    assert!(!cols("feed_terms").contains(&"label_id".to_string()));
    assert_eq!(cols("favourites"), ["id", "group_id", "category_id", "position"]);
}

#[test]
fn creating_categories_normalizes_names() {
    let s = Store::open_in_memory().unwrap();
    let a = s.create_category("Tekoäly").unwrap();
    assert_eq!(s.create_category("  tekoäly ").unwrap(), a);
    assert!(matches!(s.create_category("  "), Err(Error::Invalid(_))));
    let b = s.create_category("Sää").unwrap();
    assert!(matches!(s.rename_category(b, " TEKOÄLY"), Err(Error::Invalid(_))));
    s.rename_category(b, "Sää ja ilmasto").unwrap();
    assert!(matches!(s.rename_category(999, "X"), Err(Error::NotFound)));
    let got: Vec<String> = s.categories().unwrap().into_iter().map(|c| c.name).collect();
    assert_eq!(got, ["Sää ja ilmasto", "Tekoäly"]);
    assert_eq!(count(&s, "labels"), 0, "categories are not labels");
    let similar: Vec<CategoryId> = s.similar_categories("tekoälyn").unwrap().into_iter().map(|c| c.id).collect();
    assert_eq!(similar, vec![a]);
}

#[test]
fn mapped_term_puts_article_in_category() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(&mut s, f, &[entry("1", &["AI"]), entry("2", &["Muu"])]).unwrap();
    let c = s.create_category("Tekoäly").unwrap();
    assert_eq!(by_category(&s, c), 0);
    map_term(&s, "AI", c);
    assert_eq!(by_category(&s, c), 1);
}

#[test]
fn group_filter_returns_each_article_once() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(&mut s, f, &[entry("1", &["AI", "Robotit"]), entry("2", &["Muu"]), entry("3", &[])]).unwrap();
    let a = s.create_category("Tekoäly").unwrap();
    let b = s.create_category("Robotiikka").unwrap();
    let g = s.create_group("Aihe").unwrap();
    s.move_category(a, Some(g), 0).unwrap();
    s.move_category(b, Some(g), 1).unwrap();
    map_term(&s, "AI", a);
    map_term(&s, "Robotit", b);
    set_ai(&s, article_id(&s, "1"), a);
    set_ai(&s, article_id(&s, "3"), b);
    let q = SearchQuery { group_ids: vec![g], ..Default::default() };
    let r = s.search(&q, page()).unwrap();
    assert_eq!(r.total, 2);
    assert_eq!(r.items.len(), 2);
}

#[test]
fn manual_category_overrides_terms_and_ai() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(&mut s, f, &[entry("1", &["AI"])]).unwrap();
    let (a, b, c) = (s.create_category("A").unwrap(), s.create_category("B").unwrap(), s.create_category("C").unwrap());
    let art = article_id(&s, "1");
    map_term(&s, "AI", a);
    set_ai(&s, art, b);
    assert_eq!(
        (by_category(&s, a), by_category(&s, b), by_category(&s, c)),
        (1, 0, 0),
        "mapped terms drop the AI category"
    );
    s.set_article_category(art, Some(c)).unwrap();
    assert_eq!((by_category(&s, a), by_category(&s, b), by_category(&s, c)), (0, 0, 1));
    let counts = s.category_counts(ListFilter::default()).unwrap();
    assert_eq!(counts.categories.get(&c), Some(&1));
    assert!(!counts.categories.contains_key(&a) && !counts.categories.contains_key(&b));
    let got = s.article_category(art).unwrap();
    assert_eq!(got.manual.map(|x| x.id), Some(c));
    assert_eq!(got.ai, None, "the AI category is not in effect");
    assert_eq!(got.from_terms.iter().map(|x| x.id).collect::<Vec<_>>(), vec![a]);
}

#[test]
fn clearing_manual_returns_to_automatic() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(&mut s, f, &[entry("1", &["AI"]), entry("2", &[])]).unwrap();
    let (a, b, c) = (s.create_category("A").unwrap(), s.create_category("B").unwrap(), s.create_category("C").unwrap());
    let art = article_id(&s, "1");
    map_term(&s, "AI", a);
    set_ai(&s, art, b);
    s.set_article_category(art, Some(c)).unwrap();
    s.set_article_category(art, None).unwrap();
    assert_eq!((in_category(&s, a), in_category(&s, b)), (vec!["1".to_string()], Vec::<String>::new()));
    assert_eq!(by_category(&s, c), 0);
    let got = s.article_category(art).unwrap();
    assert_eq!((got.manual, got.ai), (None, None), "the mapped term wins over AI");
    // A manual choice alone, cleared, leaves no row behind.
    let other = article_id(&s, "2");
    s.set_article_category(other, Some(c)).unwrap();
    assert_eq!(by_category(&s, c), 1);
    s.set_article_category(other, None).unwrap();
    assert_eq!(count(&s, "article_categories"), 1);
    assert!(matches!(s.set_article_category(other, Some(999)), Err(Error::NotFound)));
    assert!(matches!(s.set_article_category(999, Some(c)), Err(Error::NotFound)));
}

#[test]
fn deleting_category_clears_article_choices_and_terms() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(&mut s, f, &[entry("1", &["AI"]), entry("2", &[]), entry("3", &[])]).unwrap();
    let x = s.create_category("X").unwrap();
    map_term(&s, "AI", x);
    s.set_article_category(article_id(&s, "2"), Some(x)).unwrap();
    set_ai(&s, article_id(&s, "3"), x);
    s.add_favourite(Favourite::Category(x)).unwrap();
    s.delete_category(x).unwrap();
    let all = matching(&s, &SearchQuery { uncategorized: true, ..Default::default() });
    assert_eq!(all, ["1", "2", "3"]);
    assert_eq!(s.unsorted_terms().unwrap().len(), 1);
    assert_eq!(count(&s, "article_categories"), 0);
    assert!(s.category_tree().unwrap().favourites.is_empty());
    assert!(matches!(s.delete_category(x), Err(Error::NotFound)));
}

#[test]
fn merge_moves_terms_and_article_choices() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(&mut s, f, &[entry("1", &["AI"]), entry("2", &[]), entry("3", &[])]).unwrap();
    let from = s.create_category("Tekoäly").unwrap();
    let into = s.create_category("Teknologia").unwrap();
    let g = s.create_group("G").unwrap();
    s.move_category(into, Some(g), 0).unwrap();
    map_term(&s, "AI", from);
    s.set_article_category(article_id(&s, "2"), Some(from)).unwrap();
    set_ai(&s, article_id(&s, "3"), from);
    s.add_favourite(Favourite::Category(from)).unwrap();
    s.merge_categories(from, into).unwrap();
    let m: i64 = s.conn().query_row("SELECT category_id FROM feed_terms WHERE name = 'AI'", [], |r| r.get(0)).unwrap();
    assert_eq!(m, into);
    assert_eq!(in_category(&s, into), ["1", "2", "3"]);
    let gid: i64 = s.conn().query_row("SELECT group_id FROM categories WHERE id = ?1", [into], |r| r.get(0)).unwrap();
    assert_eq!(gid, g);
    assert_eq!(s.categories().unwrap().len(), 1);
    assert!(s.category_tree().unwrap().favourites.is_empty());
    s.merge_categories(into, into).unwrap();
    assert!(matches!(s.merge_categories(from, into), Err(Error::NotFound)));
}

#[test]
fn label_filter_ignores_categories() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(&mut s, f, &[entry("1", &["AI"]), entry("2", &[]), entry("3", &[])]).unwrap();
    let l = s.create_label("Tekoäly").unwrap();
    let c = s.create_category("Tekoäly").unwrap();
    assert_eq!(l, c, "same id in both tables makes a leak visible");
    map_term(&s, "AI", c);
    s.set_article_category(article_id(&s, "3"), Some(c)).unwrap();
    s.tag_article(article_id(&s, "2"), l, Origin::User, None).unwrap();
    s.tag_article(article_id(&s, "3"), l, Origin::Ai, None).unwrap();
    assert_eq!(matching(&s, &SearchQuery { label_ids: vec![l], ..Default::default() }), ["2"]);
    let with_suggested = SearchQuery { label_ids: vec![l], include_suggested: true, ..Default::default() };
    assert_eq!(matching(&s, &with_suggested), ["2", "3"]);
    assert_eq!(in_category(&s, c), ["1", "3"], "labels do not count for categories");
    assert!(s.article_labels(article_id(&s, "1")).unwrap().is_empty());
}

// ---- organising API ----------------------------------------------------

#[test]
fn tree_orders_groups_and_categories_by_position() {
    let mut s = Store::open_in_memory().unwrap();
    let g1 = s.create_group("Zeta").unwrap();
    let g2 = s.create_group("Alpha").unwrap();
    let b = s.create_category("b").unwrap();
    let a = s.create_category("a").unwrap();
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
fn tree_never_lists_labels() {
    let s = Store::open_in_memory().unwrap();
    s.create_label("Vain tunniste").unwrap();
    let t = s.category_tree().unwrap();
    assert!(t.ungrouped.is_empty() && t.groups.is_empty());
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
    let l = s.create_category("L").unwrap();
    s.move_category(l, Some(g), 0).unwrap();
    s.add_favourite(Favourite::Group(g)).unwrap();
    s.delete_group(g).unwrap();
    let t = s.category_tree().unwrap();
    assert!(t.groups.is_empty());
    assert_eq!(names(&t.ungrouped), vec!["L"]);
    assert!(t.favourites.is_empty());
}

#[test]
fn new_categories_go_last_and_a_deleted_group_appends_its_own() {
    let mut s = Store::open_in_memory().unwrap();
    let b = s.create_category("b").unwrap();
    s.create_category("a").unwrap();
    assert_eq!(names(&s.category_tree().unwrap().ungrouped), vec!["b", "a"], "in the order made");
    let g = s.create_group("G").unwrap();
    let d = s.create_category("d").unwrap();
    let c = s.create_category("c").unwrap();
    s.move_category(d, Some(g), 0).unwrap();
    s.move_category(c, Some(g), 1).unwrap();
    s.delete_group(g).unwrap();
    let t = s.category_tree().unwrap();
    assert_eq!(names(&t.ungrouped), vec!["b", "a", "d", "c"], "after the Ungrouped ones, in group order");
    let positions: Vec<i64> = {
        let mut st = s.conn().prepare("SELECT position FROM categories ORDER BY position").unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
    };
    assert_eq!(positions, vec![0, 1, 2, 3], "no ties");
    assert_eq!(s.create_category(" B ").unwrap(), b, "an existing name keeps its place");
    assert!(matches!(s.delete_group(g), Err(Error::NotFound)));
}

#[test]
fn move_category_between_groups_renumbers_both() {
    let mut s = Store::open_in_memory().unwrap();
    let g1 = s.create_group("G1").unwrap();
    let g2 = s.create_group("G2").unwrap();
    let ids: Vec<_> = ["a", "b", "c", "d"].iter().map(|n| s.create_category(n).unwrap()).collect();
    for (i, l) in ids.iter().take(3).enumerate() {
        s.move_category(*l, Some(g1), i).unwrap();
    }
    s.move_category(ids[3], Some(g2), 0).unwrap();
    s.move_category(ids[1], Some(g2), 0).unwrap();
    let t = s.category_tree().unwrap();
    assert_eq!(names(&t.groups[0].categories), vec!["a", "c"]);
    assert_eq!(names(&t.groups[1].categories), vec!["b", "d"]);
    let pos = |l: CategoryId| -> i64 {
        s.conn().query_row("SELECT position FROM categories WHERE id = ?1", [l], |r| r.get(0)).unwrap()
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
    ingest_unsorted(&mut s, f, &[entry("1", &["AI", "Sports", "Misc"])]).unwrap();
    let l = s.create_category("Tech").unwrap();
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
fn unsorted_terms_come_most_frequent_first() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(
        &mut s,
        f,
        &[entry("1", &["Aa", "Harvinainen"]), entry("2", &["Yleinen", "Bb"]), entry("3", &["Yleinen", "Bb"])],
    )
    .unwrap();
    ingest_unsorted(&mut s, f, &[entry("4", &["Yleinen"])]).unwrap();
    let names: Vec<String> = s.unsorted_terms().unwrap().into_iter().map(|t| t.name).collect();
    assert_eq!(names, ["Yleinen", "Bb", "Aa", "Harvinainen"], "by article count, then by name");
}

#[test]
fn favourites_keep_their_order() {
    let mut s = Store::open_in_memory().unwrap();
    let g = s.create_group("G").unwrap();
    let l = s.create_category("L").unwrap();
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
    ingest_unsorted(&mut s, f, &[entry("1", &["AI"]), entry("2", &[])]).unwrap();
    let g = s.create_group("G").unwrap();
    let a = s.create_category("A").unwrap();
    let b = s.create_category("B").unwrap();
    s.move_category(a, Some(g), 0).unwrap();
    s.move_category(b, Some(g), 1).unwrap();
    let art = article_id(&s, "1");
    set_ai(&s, art, a);
    set_ai(&s, article_id(&s, "2"), a);
    s.map_term(term_id(&s, "AI"), Some(b)).unwrap();
    let c = s.category_counts(ListFilter::default()).unwrap();
    assert_eq!(c.groups[&g], 2, "articles 1 (term) and 2 (AI)");
    assert_eq!((c.categories[&a], c.categories[&b]), (1, 1));
    assert_eq!(c.uncategorized, 0);
    s.set_read(art, true).unwrap();
    s.set_read(article_id(&s, "2"), true).unwrap();
    let unread = ListFilter { view: View::Unread, ..Default::default() };
    let c = s.category_counts(unread).unwrap();
    assert_eq!(c.groups.get(&g).copied().unwrap_or(0), 0);
    let other = ListFilter { feed_id: Some(f2), ..Default::default() };
    assert!(s.category_counts(other).unwrap().groups.is_empty());
    let own = ListFilter { feed_id: Some(f), ..Default::default() };
    assert_eq!(s.category_counts(own).unwrap().groups[&g], 2);
}

#[test]
fn counts_follow_manual_choices() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(&mut s, f, &[entry("1", &["AI"]), entry("2", &["AI"])]).unwrap();
    let g = s.create_group("G").unwrap();
    let l = s.create_category("L").unwrap();
    let other = s.create_category("Other").unwrap();
    s.move_category(l, Some(g), 0).unwrap();
    s.map_term(term_id(&s, "AI"), Some(l)).unwrap();
    let c = s.category_counts(ListFilter::default()).unwrap();
    assert_eq!((c.categories[&l], c.groups[&g]), (2, 2));
    s.set_article_category(article_id(&s, "1"), Some(other)).unwrap();
    let c = s.category_counts(ListFilter::default()).unwrap();
    assert_eq!((c.categories[&l], c.groups[&g], c.categories[&other]), (1, 1, 1));
    s.set_article_category(article_id(&s, "2"), Some(other)).unwrap();
    let c = s.category_counts(ListFilter::default()).unwrap();
    assert!(!c.categories.contains_key(&l) && !c.groups.contains_key(&g));
    assert_eq!(c.categories[&other], 2);
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
    ingest_unsorted(&mut s, f1, &[entry("1", &["AI"])]).unwrap();
    ingest_unsorted(&mut s, f2, &[entry("2", &["AI"])]).unwrap();
    let g = s.create_group("G").unwrap();
    let l = s.create_category("L").unwrap();
    s.move_category(l, Some(g), 0).unwrap();
    s.conn().execute("UPDATE feed_terms SET category_id = ?1", [l]).unwrap();
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

#[test]
fn marking_a_category_read_leaves_other_articles_unread() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(&mut s, f, &[entry("1", &["AI"]), entry("2", &["Muu"]), entry("3", &[])]).unwrap();
    let l = s.create_category("Tekoäly").unwrap();
    map_term(&s, "AI", l);
    s.set_article_category(article_id(&s, "3"), Some(l)).unwrap();
    let q = SearchQuery { category_ids: vec![l], feed_ids: vec![f], ..Default::default() };
    assert_eq!(s.mark_query_read(&q).unwrap(), 2);
    let read = |guid: &str| s.article(article_id(&s, guid)).unwrap().is_read;
    assert!(read("1") && read("3"));
    assert!(!read("2"));
    assert_eq!(s.mark_query_read(&q).unwrap(), 0, "already read");
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
        ingest_unsorted(&mut s, f, &[entry("1", names)]).unwrap();
        s
    }

    fn group_of(s: &Store, c: CategoryId) -> Option<GroupId> {
        s.conn().query_row("SELECT group_id FROM categories WHERE id = ?1", [c], |r| r.get(0)).unwrap()
    }

    fn category_of(s: &Store, t: TermId) -> Option<CategoryId> {
        s.conn().query_row("SELECT category_id FROM feed_terms WHERE id = ?1", [t], |r| r.get(0)).unwrap()
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
        let l = s.create_category("L").unwrap();
        s.create_label("Not a category").unwrap();
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
    fn plan_input_sends_the_most_frequent_terms_first() {
        let mut s = Store::open_in_memory().unwrap();
        let f = feed(&mut s, "https://a.example/rss");
        // "Vanha" is seen first but in one article; "Uusi" later but in three. Ties go by name.
        ingest_unsorted(&mut s, f, &[entry("1", &["Vanha"])]).unwrap();
        ingest_unsorted(&mut s, f, &[entry("2", &["Uusi"]), entry("3", &["Uusi"]), entry("4", &["Uusi", "Toinen"])])
            .unwrap();
        let input = s.category_plan_input(2).unwrap();
        let names: Vec<&str> = input.terms.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Uusi", "Toinen"]);
        assert_eq!(input.more_terms, 1);
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
        let loose = s.create_category("Loose").unwrap();
        let existing = s.create_group("Old").unwrap();
        let d = CategoryDraft {
            new_groups: vec![ng(1, "Politiikka")],
            new_categories: vec![nc(1, "Vaalit", GroupRef::New(1)), nc(2, "Muu", GroupRef::Existing(existing))],
            placements: vec![Placement {
                category: loose,
                rename: Some("Vapaa".into()),
                group: GroupRef::New(1),
                ticked: true,
            }],
            mappings: vec![mp(a, CategoryRef::New(1)), mp(b, CategoryRef::Existing(loose))],
            vague: vec![],
            grouping_skipped: false,
        };
        let id = s.save_category_draft(&d, Provider::Openai, "m").unwrap();
        let r = s.apply_category_draft(id, &d).unwrap();
        assert_eq!((r.applied, r.skipped), (6, 0));
        let tree = s.category_tree().unwrap();
        let pol = tree.groups.iter().find(|g| g.group.name == "Politiikka").unwrap();
        let names: Vec<_> = pol.categories.iter().map(|c| c.category.name.as_str()).collect();
        assert_eq!(names, vec!["Vaalit", "Vapaa"]);
        assert_eq!(category_of(&s, a), Some(pol.categories[0].category.id));
        assert_eq!(category_of(&s, b), Some(loose));
        assert_eq!(tree.groups.last().unwrap().group.name, "Politiikka");
        assert!(s.pending_category_draft().unwrap().is_none());
        assert_eq!(count(&s, "labels"), 0, "the draft never makes labels");
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
        assert_eq!(count(&s, "categories"), 0);
        assert_eq!(category_of(&s, a), None);
    }

    #[test]
    fn apply_skips_stale_items() {
        let mut s = setup(&["a", "b"]);
        let (a, b) = (term_named(&s, "a"), term_named(&s, "b"));
        let gone = s.create_group("Gone").unwrap();
        let keep = s.create_group("Keep").unwrap();
        let p1 = s.create_category("P1").unwrap();
        let p2 = s.create_category("P2").unwrap();
        let d = CategoryDraft {
            placements: vec![
                Placement { category: p1, rename: None, group: GroupRef::Existing(gone), ticked: true },
                Placement { category: p2, rename: None, group: GroupRef::Existing(keep), ticked: true },
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
        assert_eq!(category_of(&s, b), Some(p1));
        assert_eq!(category_of(&s, a), Some(p2));
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
        assert_eq!((count(&s, "category_groups"), count(&s, "categories")), (1, 1));
        assert_eq!(category_of(&s, b), None);
        // Discarded: Approve afterwards changes nothing either.
        let id = s.save_category_draft(&again, Provider::Openai, "m").unwrap();
        s.discard_category_draft(id).unwrap();
        assert!(matches!(s.apply_category_draft(id, &again), Err(juttusihti_core::Error::NotFound)));
        assert_eq!(category_of(&s, b), None);
    }

    #[test]
    fn a_new_group_whose_contents_are_all_stale_is_not_created() {
        let mut s = setup(&["a"]);
        let loose = s.create_category("Loose").unwrap();
        let other = s.create_group("Other").unwrap();
        let d = CategoryDraft {
            new_groups: vec![ng(1, "Uusi")],
            placements: vec![Placement { category: loose, rename: None, group: GroupRef::New(1), ticked: true }],
            ..Default::default()
        };
        let id = s.save_category_draft(&d, Provider::Openai, "m").unwrap();
        s.move_category(loose, Some(other), 0).unwrap(); // grouped by hand meanwhile
        let r = s.apply_category_draft(id, &d).unwrap();
        assert_eq!((r.applied, r.skipped), (0, 1));
        assert_eq!(count(&s, "category_groups"), 1, "no empty group is left behind");
    }

    #[test]
    fn an_unreadable_stored_draft_counts_as_none() {
        let s = setup(&["a"]);
        let id = s
            .record_suggestion(&juttusihti_core::research::NewSuggestion {
                session_id: None,
                kind: DRAFT_KIND.into(),
                payload: serde_json::json!({"from": "an older version"}),
                provider: "openai".into(),
                model: "m".into(),
                prompt_version: DRAFT_PROMPT_VERSION.into(),
                confidence: None,
                evidence: None,
            })
            .unwrap();
        assert_eq!(s.pending_category_draft().unwrap(), None);
        let status: String =
            s.conn().query_row("SELECT status FROM ai_suggestions WHERE id = ?1", [id], |r| r.get(0)).unwrap();
        assert_eq!(status, "rejected", "set aside so it is not read again");
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
        let l = category_of(&s, a).unwrap();
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
        assert_eq!((count(&s, "category_groups"), count(&s, "categories")), (0, 0));
        assert_eq!(category_of(&s, a), None);
    }
}

#[test]
fn draft_is_empty_only_without_proposals() {
    use juttusihti_core::category_draft::{CategoryDraft, GroupRef, Placement};
    let mut d = CategoryDraft { vague: vec![1, 2], ..Default::default() };
    assert!(d.is_empty(), "vague terms alone propose nothing");
    d.placements.push(Placement { category: 1, rename: None, group: GroupRef::New(1), ticked: true });
    assert!(!d.is_empty());
}

/// (a) manual category, (b) mapped term, (c) AI category, (d) only a confirmed
/// label, (e) nothing, (f) an unsorted term.
fn uncategorized_data() -> Store {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(
        &mut s,
        f,
        &[
            entry("a", &[]),
            entry("b", &["AI"]),
            entry("c", &[]),
            entry("d", &[]),
            entry("e", &[]),
            entry("f", &["Odd"]),
        ],
    )
    .unwrap();
    let l = s.create_category("L").unwrap();
    let m = s.create_category("M").unwrap();
    s.set_article_category(article_id(&s, "a"), Some(l)).unwrap();
    s.map_term(term_id(&s, "AI"), Some(m)).unwrap();
    set_ai(&s, article_id(&s, "c"), l);
    let label = s.create_label("Tunniste").unwrap();
    s.tag_article(article_id(&s, "d"), label, Origin::User, None).unwrap();
    s
}

#[test]
fn uncategorized_filter_excludes_categorized_articles() {
    let s = uncategorized_data();
    let q = SearchQuery { uncategorized: true, ..Default::default() };
    assert_eq!(matching(&s, &q), ["d", "e", "f"]);
    assert_eq!(s.search(&q, page()).unwrap().total, 3);
}

#[test]
fn counts_include_uncategorized_and_follow_the_view() {
    let s = uncategorized_data();
    assert_eq!(s.category_counts(ListFilter::default()).unwrap().uncategorized, 3);
    s.set_read(article_id(&s, "e"), true).unwrap();
    let unread = ListFilter { view: View::Unread, ..Default::default() };
    assert_eq!(s.category_counts(unread).unwrap().uncategorized, 2);
}

/// (a) unsorted term, (b) mapped term, (c) only an ignored term, (d) nothing,
/// (e) no terms but a manual category.
fn without_terms_data() -> Store {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(
        &mut s,
        f,
        &[entry("a", &["Odd"]), entry("b", &["AI"]), entry("c", &["Tilaajille"]), entry("d", &[]), entry("e", &[])],
    )
    .unwrap();
    let l = s.create_category("L").unwrap();
    let m = s.create_category("M").unwrap();
    s.map_term(term_id(&s, "AI"), Some(m)).unwrap();
    s.set_term_ignored(term_id(&s, "Tilaajille"), true).unwrap();
    s.set_article_category(article_id(&s, "e"), Some(l)).unwrap();
    s
}

#[test]
fn without_terms_filter_excludes_articles_with_mapped_or_unsorted_terms() {
    let s = without_terms_data();
    let q = SearchQuery { uncategorized: true, without_terms: true, ..Default::default() };
    assert_eq!(matching(&s, &q), ["c", "d"]);
}

// ---- AI categorizing of uncategorized articles ----------------------------------

fn dated(guid: &str, published_at: i64, terms: &[&str]) -> NewArticle {
    NewArticle { published_at: Some(published_at), ..entry(guid, terms) }
}

/// (old) no terms, (mid) no terms, (new) no terms, (termed) an unsorted
/// publisher term, (categorized) no terms but a manual category. Newest first:
/// new, mid, old.
fn to_categorize_data() -> Store {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(
        &mut s,
        f,
        &[
            dated("old", 1_000, &[]),
            dated("mid", 2_000, &[]),
            dated("new", 3_000, &[]),
            dated("termed", 4_000, &["Odd"]),
            dated("categorized", 5_000, &[]),
        ],
    )
    .unwrap();
    let l = s.create_category("L").unwrap();
    s.set_article_category(article_id(&s, "categorized"), Some(l)).unwrap();
    s
}

fn guids(s: &Store, items: &[ArticleForCategorizing]) -> Vec<String> {
    items.iter().map(|a| s.article(a.id).unwrap().guid).collect()
}

#[test]
fn articles_to_categorize_are_uncategorized_without_terms_and_newest_first() {
    let s = to_categorize_data();
    let (items, eligible) = s.articles_to_categorize(ListFilter::default(), 100).unwrap();
    assert_eq!(guids(&s, &items), ["new", "mid", "old"]);
    assert_eq!(eligible, 3);
    assert_eq!(items[0].title, "Title new");
    assert_eq!(items[0].text, "Body");
    let (items, eligible) = s.articles_to_categorize(ListFilter::default(), 2).unwrap();
    assert_eq!(guids(&s, &items), ["new", "mid"]);
    assert_eq!(eligible, 3, "the total counts every eligible article");
}

#[test]
fn ai_eligibility_skips_unsorted_terms() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(
        &mut s,
        f,
        &[
            dated("none", 1_000, &[]),
            dated("ignored", 2_000, &["Tilaajille"]),
            dated("unsorted", 3_000, &["Odd"]),
            dated("mapped", 4_000, &["AI"]),
        ],
    )
    .unwrap();
    let c = s.create_category("Tekoäly").unwrap();
    s.map_term(term_id(&s, "AI"), Some(c)).unwrap();
    s.set_term_ignored(term_id(&s, "Tilaajille"), true).unwrap();
    let (items, eligible) = s.articles_to_categorize(ListFilter::default(), 100).unwrap();
    assert_eq!(guids(&s, &items), ["ignored", "none"]);
    assert_eq!(eligible, 2);
}

#[test]
fn articles_to_categorize_skip_checked_ones_and_follow_the_filter() {
    let mut s = to_categorize_data();
    let new = article_id(&s, "new");
    s.apply_article_categories(&[], &[new]).unwrap();
    let (items, eligible) = s.articles_to_categorize(ListFilter::default(), 100).unwrap();
    assert_eq!(guids(&s, &items), ["mid", "old"]);
    assert_eq!(eligible, 2);

    s.set_read(article_id(&s, "mid"), true).unwrap();
    let unread = ListFilter { view: View::Unread, ..Default::default() };
    assert_eq!(guids(&s, &s.articles_to_categorize(unread, 100).unwrap().0), ["old"]);

    let other = feed(&mut s, "https://b.example/rss");
    ingest_unsorted(&mut s, other, &[dated("elsewhere", 9_000, &[])]).unwrap();
    let only_other = ListFilter { feed_id: Some(other), ..Default::default() };
    let (items, eligible) = s.articles_to_categorize(only_other, 100).unwrap();
    assert_eq!((guids(&s, &items), eligible), (vec!["elsewhere".to_string()], 1));
}

#[test]
fn articles_to_categorize_fall_back_to_the_start_of_the_content() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    let long = "word ".repeat(200);
    ingest_unsorted(&mut s, f, &[NewArticle { summary: None, content: Some(long), ..entry("a", &[]) }]).unwrap();
    let (items, _) = s.articles_to_categorize(ListFilter::default(), 100).unwrap();
    assert!(items[0].text.starts_with("word word"));
    assert!(items[0].text.chars().count() <= 300);
}

fn choices(s: &Store, a: ArticleId) -> (Option<CategoryId>, Option<CategoryId>) {
    s.conn()
        .query_row("SELECT user_category_id, ai_category_id FROM article_categories WHERE article_id = ?1", [a], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap_or((None, None))
}

#[test]
fn apply_writes_one_ai_category_and_never_manual() {
    let mut s = to_categorize_data();
    let (new, mid, old) = (article_id(&s, "new"), article_id(&s, "mid"), article_id(&s, "old"));
    let space = s.create_category("Space").unwrap();
    let mine = s.create_category("Mine").unwrap();
    // The user chose a category for `mid` by hand: AI must not change that.
    s.set_article_category(mid, Some(mine)).unwrap();

    let ai = |n: &str| Some(AiCategory { name: n.into(), group: None });
    let results = vec![(new, ai("  space ")), (mid, ai("Tiede")), (old, ai("  "))];
    let sent = [new, mid, old];
    let stats = s.apply_article_categories(&results, &sent).unwrap();
    assert_eq!(stats.new_categories, 1, "Tiede is created");
    assert_eq!(stats.categorized, 2);
    let tiede = s.categories().unwrap().into_iter().find(|c| c.name == "Tiede").unwrap().id;
    let tree = s.category_tree().unwrap();
    assert_eq!(tree.ungrouped.last().unwrap().category.id, tiede, "new ones go last in Ungrouped");

    assert_eq!(choices(&s, new), (None, Some(space)));
    assert_eq!(choices(&s, mid), (Some(mine), Some(tiede)), "the manual choice stays");
    assert_eq!(choices(&s, old), (None, None));
    assert_eq!(in_category(&s, tiede), Vec::<String>::new(), "manual wins over AI");
    assert_eq!(in_category(&s, space), ["new"]);
    assert_eq!(count(&s, "labels"), 0, "AI categories are not labels");
    assert_eq!(count(&s, "article_labels"), 0);
    assert_eq!(count(&s, "category_ai_checks"), 3, "every sent article is checked, also `old`");
    assert!(s.articles_to_categorize(ListFilter::default(), 100).unwrap().0.is_empty());

    let again = s.apply_article_categories(&results, &sent).unwrap();
    assert_eq!(again.new_categories, 0);
    assert_eq!(count(&s, "categories"), 4, "L, Space, Mine and Tiede");
    assert_eq!(count(&s, "category_ai_checks"), 3);
    // A later result replaces the earlier AI choice.
    s.apply_article_categories(&[(new, ai("Tiede"))], &[new]).unwrap();
    assert_eq!(choices(&s, new), (None, Some(tiede)));
}

#[test]
fn apply_files_new_category_into_named_group() {
    let mut s = to_categorize_data();
    let (new, mid, old) = (article_id(&s, "new"), article_id(&s, "mid"), article_id(&s, "old"));
    let sport = s.create_group("Sport").unwrap();
    let science = s.create_group("Science").unwrap();
    let existing = s.create_category("Space").unwrap(); // ungrouped
    let ai = |n: &str, g| Some(AiCategory { name: n.into(), group: g });
    let results = vec![
        (new, ai("Jalkapallo", Some(sport))),
        (mid, ai("jalkapallo", Some(science))), // same new name, other group: first wins
        (old, ai("space", Some(science))),      // existing: never moved
    ];
    s.apply_article_categories(&results, &[new, mid, old]).unwrap();
    let tree = s.category_tree().unwrap();
    let in_group = |g| {
        tree.groups
            .iter()
            .find(|n| n.group.id == g)
            .unwrap()
            .categories
            .iter()
            .map(|c| c.category.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(in_group(sport), ["Jalkapallo"]);
    assert!(in_group(science).is_empty());
    assert!(tree.ungrouped.iter().any(|c| c.category.id == existing), "Space stays ungrouped");
    // A group deleted meanwhile means Ungrouped.
    let gone = s.create_group("Gone").unwrap();
    s.delete_group(gone).unwrap();
    s.apply_article_categories(&[(new, ai("Tennis", Some(gone)))], &[new]).unwrap();
    let tree = s.category_tree().unwrap();
    assert_eq!(tree.ungrouped.last().unwrap().category.name, "Tennis");
    assert_eq!(
        s.category_vocabulary().unwrap().categories.iter().find(|c| c.name == "Jalkapallo").unwrap().group_id,
        Some(sport)
    );
}

#[test]
fn apply_skips_articles_deleted_meanwhile() {
    let mut s = to_categorize_data();
    let (new, mid, old) = (article_id(&s, "new"), article_id(&s, "mid"), article_id(&s, "old"));
    s.conn().execute("DELETE FROM articles WHERE id = ?1", [mid]).unwrap();
    let ai = |n: &str| Some(AiCategory { name: n.into(), group: None });
    let results = vec![(new, ai("Space")), (mid, ai("Tiede")), (old, None)];
    let stats = s.apply_article_categories(&results, &[new, mid, old]).unwrap();
    assert_eq!(stats.categorized, 1);
    assert_eq!(stats.new_categories, 1, "only Space; the deleted article's Tiede is not created");
    assert_eq!(count(&s, "category_ai_checks"), 2, "new and old are checked, the deleted one is not");
    assert_eq!(in_category(&s, s.create_category("Space").unwrap()), ["new"]);
}

#[test]
fn create_category_checked_says_whether_it_was_created() {
    let s = Store::open_in_memory().unwrap();
    let (a, created) = s.create_category_checked("Tekoäly").unwrap();
    assert!(created);
    assert_eq!(s.create_category_checked("  tekoäly ").unwrap(), (a, false));
}

// ---- feed default category ---------------------------------------------------

/// A feed `f` with a default category sport: articles "1" (no terms), "2" (an
/// unsorted term) and "3" (a term mapped to tech).
fn default_data() -> (Store, FeedId, CategoryId, CategoryId) {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(&mut s, f, &[entry("1", &[]), entry("2", &["Misc"]), entry("3", &["AI"])]).unwrap();
    let sport = s.create_category("Sport").unwrap();
    let tech = s.create_category("Tech").unwrap();
    map_term(&s, "AI", tech);
    s.set_feed_default_category(f, Some(sport)).unwrap();
    (s, f, sport, tech)
}

#[test]
fn feed_default_covers_articles_without_mapped_terms() {
    let (s, _, sport, tech) = default_data();
    assert_eq!(in_category(&s, sport), ["1", "2"]);
    assert_eq!(in_category(&s, tech), ["3"]);
    assert_eq!(by_category(&s, sport), 2);
    let counts = s.category_counts(ListFilter::default()).unwrap();
    assert_eq!(counts.categories.get(&sport), Some(&2));
    assert_eq!(counts.categories.get(&tech), Some(&1));
    assert_eq!(counts.uncategorized, 0);
}

#[test]
fn feed_default_beats_ai_and_clearing_it_brings_ai_back() {
    let (s, f, sport, tech) = default_data();
    set_ai(&s, article_id(&s, "1"), tech);
    assert_eq!(in_category(&s, sport), ["1", "2"]);
    assert_eq!(in_category(&s, tech), ["3"]);
    s.set_feed_default_category(f, None).unwrap();
    assert_eq!(in_category(&s, tech), ["1", "3"]);
    assert!(in_category(&s, sport).is_empty());
}

#[test]
fn mapped_terms_drop_the_ai_category() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    ingest_unsorted(&mut s, f, &[entry("1", &["AI"])]).unwrap();
    let tech = s.create_category("Tech").unwrap();
    let local = s.create_category("Local").unwrap();
    map_term(&s, "AI", tech);
    set_ai(&s, article_id(&s, "1"), local);
    assert_eq!(in_category(&s, tech), ["1"]);
    assert!(in_category(&s, local).is_empty());
    let counts = s.category_counts(ListFilter::default()).unwrap();
    assert_eq!(counts.categories.get(&local), None);
}

#[test]
fn manual_category_beats_feed_default() {
    let (s, _, sport, _) = default_data();
    let local = s.create_category("Local").unwrap();
    s.set_article_category(article_id(&s, "1"), Some(local)).unwrap();
    assert_eq!(in_category(&s, local), ["1"]);
    assert_eq!(in_category(&s, sport), ["2"]);
}

#[test]
fn feed_default_articles_are_not_offered_to_ai() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    let g = feed(&mut s, "https://b.example/rss");
    ingest_unsorted(&mut s, f, &[entry("1", &[])]).unwrap();
    ingest_unsorted(&mut s, g, &[entry("2", &[])]).unwrap();
    let sport = s.create_category("Sport").unwrap();
    s.set_feed_default_category(f, Some(sport)).unwrap();
    let (offered, eligible) = s.articles_to_categorize(ListFilter::default(), 100).unwrap();
    assert_eq!(eligible, 1);
    assert!(offered.iter().all(|a| s.article(a.id).unwrap().feed_id == g));
}

#[test]
fn setting_a_default_needs_an_existing_feed() {
    let s = Store::open_in_memory().unwrap();
    assert!(matches!(s.set_feed_default_category(99, None), Err(Error::NotFound)));
}

#[test]
fn deleting_or_merging_the_default_category() {
    let (mut s, f, sport, tech) = default_data();
    s.merge_categories(sport, tech).unwrap();
    assert_eq!(s.feed(f).unwrap().default_category_id, Some(tech));
    s.delete_category(tech).unwrap();
    assert_eq!(s.feed(f).unwrap().default_category_id, None);
}

#[test]
fn article_category_reports_the_feed_default() {
    let (s, _, sport, tech) = default_data();
    set_ai(&s, article_id(&s, "1"), tech);
    let one = s.article_category(article_id(&s, "1")).unwrap();
    let from_feed = one.from_feed.unwrap();
    assert_eq!((from_feed.category.id, from_feed.feed_title.as_str()), (sport, "https://a.example/rss"));
    assert_eq!(one.ai, None, "the AI category is not in effect");
    let three = s.article_category(article_id(&s, "3")).unwrap();
    assert_eq!(three.from_feed, None);
    assert_eq!(three.from_terms.len(), 1);
    s.set_article_category(article_id(&s, "1"), Some(tech)).unwrap();
    assert_eq!(s.article_category(article_id(&s, "1")).unwrap().from_feed, None);
}

#[test]
fn list_and_counts_agree_on_every_category() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    let g = feed(&mut s, "https://b.example/rss");
    ingest_unsorted(
        &mut s,
        f,
        &[entry("1", &[]), entry("2", &["Misc"]), entry("3", &["AI"]), entry("4", &[]), entry("5", &["AI"])],
    )
    .unwrap();
    ingest_unsorted(&mut s, g, &[entry("6", &[]), entry("7", &["Misc"]), entry("8", &[])]).unwrap();
    let sport = s.create_category("Sport").unwrap();
    let tech = s.create_category("Tech").unwrap();
    let local = s.create_category("Local").unwrap();
    s.create_category("Empty").unwrap();
    map_term(&s, "AI", tech);
    s.set_feed_default_category(f, Some(sport)).unwrap();
    s.set_article_category(article_id(&s, "4"), Some(local)).unwrap();
    set_ai(&s, article_id(&s, "5"), local);
    set_ai(&s, article_id(&s, "6"), local);
    set_ai(&s, article_id(&s, "7"), tech);
    let counts = s.category_counts(ListFilter::default()).unwrap();
    for c in s.categories().unwrap() {
        let counted = counts.categories.get(&c.id).copied().unwrap_or(0);
        assert_eq!(by_category(&s, c.id), counted, "{}", c.name);
    }
    assert_eq!(in_category(&s, local), ["4", "6"]);
    assert_eq!(in_category(&s, tech), ["3", "5", "7"]);
    assert_eq!(counts.uncategorized, 1, "only article 8");
}

// ---- feeds in groups -----------------------------------------------------------

/// Group sports with category football; politics in no group. Feed f is in
/// sports: its "1" is mapped to politics, "2" has no category. Feed g (no group)
/// has "3" mapped to football.
fn group_data() -> (Store, FeedId, FeedId, GroupId, CategoryId, CategoryId) {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://f.example/rss");
    let g = feed(&mut s, "https://g.example/rss");
    ingest_unsorted(&mut s, f, &[entry("1", &["Pol"]), entry("2", &[])]).unwrap();
    ingest_unsorted(&mut s, g, &[entry("3", &["Foot"])]).unwrap();
    let sports = s.create_group("Sports").unwrap();
    let football = s.create_category("Football").unwrap();
    let politics = s.create_category("Politics").unwrap();
    s.move_category(football, Some(sports), 0).unwrap();
    map_term(&s, "Foot", football);
    map_term(&s, "Pol", politics);
    s.set_feed_group(f, Some(sports)).unwrap();
    (s, f, g, sports, football, politics)
}

#[test]
fn a_group_contains_all_articles_of_its_feeds() {
    let (s, f, _, sports, _, _) = group_data();
    let in_group = matching(&s, &SearchQuery { group_ids: vec![sports], ..Default::default() });
    assert_eq!(in_group, ["1", "2", "3"]);
    let c = s.category_counts(ListFilter::default()).unwrap();
    assert_eq!(c.groups.get(&sports), Some(&3));
    assert_eq!(c.feeds.get(&f), Some(&2));
    assert_eq!(c.uncategorized, 1); // "2": in the group only through its feed
}

#[test]
fn a_group_counts_an_article_once_via_feed_and_category() {
    let (mut s, f, _, sports, football, _) = group_data();
    // "2" is football and, through feed f, in sports; "1" maps to two categories of sports.
    s.conn().execute("UPDATE feed_terms SET category_id = ?1 WHERE name = 'Pol'", [football]).unwrap();
    let hockey = s.create_category("Hockey").unwrap();
    s.move_category(hockey, Some(sports), 1).unwrap();
    s.set_article_category(article_id(&s, "2"), Some(football)).unwrap();
    ingest_unsorted(&mut s, f, &[entry("4", &["Pol", "Puck"])]).unwrap();
    map_term(&s, "Puck", hockey);
    let c = s.category_counts(ListFilter::default()).unwrap();
    assert_eq!(c.groups.get(&sports), Some(&4)); // 1, 2, 4 via feed f; 3 via football
    assert_eq!(matching(&s, &SearchQuery { group_ids: vec![sports], ..Default::default() }).len(), 4);
}

#[test]
fn group_feed_counts_follow_the_scope() {
    let (s, f, g, sports, _, _) = group_data();
    let c = s.category_counts(ListFilter { feed_id: Some(g), ..Default::default() }).unwrap();
    assert_eq!(c.feeds.get(&f).copied().unwrap_or(0), 0);
    assert_eq!(c.groups.get(&sports), Some(&1));
}

#[test]
fn tree_lists_group_feeds_and_default_users_by_title() {
    let mut s = Store::open_in_memory().unwrap();
    let b = titled(&mut s, "b.example");
    let a = titled(&mut s, "A.example");
    let c = titled(&mut s, "c.example");
    let sports = s.create_group("Sports").unwrap();
    let football = s.create_category("Football").unwrap();
    s.move_category(football, Some(sports), 0).unwrap();
    s.set_feed_group(b, Some(sports)).unwrap();
    s.set_feed_group(a, Some(sports)).unwrap();
    s.set_feed_default_category(c, Some(football)).unwrap();
    let t = s.category_tree().unwrap();
    let titles = |v: &[GroupFeed]| v.iter().map(|f| f.title.clone()).collect::<Vec<_>>();
    assert_eq!(titles(&t.groups[0].feeds), ["A.example", "b.example"]);
    assert_eq!(t.groups[0].categories[0].default_for, ["c.example"]);
    assert_eq!(titles(&t.ungrouped_feeds), ["c.example"]);
}

#[test]
fn deleting_a_group_releases_its_feeds() {
    let (mut s, f, _, sports, _, _) = group_data();
    s.delete_group(sports).unwrap();
    assert_eq!(s.feed(f).unwrap().category_group_id, None);
    assert!(s.category_tree().unwrap().groups.is_empty());
}

#[test]
fn set_feed_group_rejects_missing_feed_or_group() {
    let (s, f, _, _, _, _) = group_data();
    assert!(matches!(s.set_feed_group(99, None), Err(Error::NotFound)));
    assert!(matches!(s.set_feed_group(f, Some(99)), Err(Error::NotFound)));
}

#[test]
fn find_or_create_group_reuses_by_normalized_name() {
    let s = Store::open_in_memory().unwrap();
    let a = s.find_or_create_group("Urheilu").unwrap();
    assert_eq!(s.find_or_create_group(" urheilu ").unwrap(), a);
    assert_ne!(s.find_or_create_group("Politiikka").unwrap(), a);
}

#[test]
fn category_feed_id_intersects_with_the_feed_scope() {
    let (s, f, g, _, _, _) = group_data();
    let q = SearchQuery { feed_ids: vec![g], category_feed_id: Some(f), ..Default::default() };
    assert!(matching(&s, &q).is_empty());
    let q = SearchQuery { category_feed_id: Some(f), ..Default::default() };
    assert_eq!(matching(&s, &q), ["1", "2"]);
}

/// Counts are built by their own statement; under every scope they must match
/// what the list finds for each category, each group, each group feed and
/// Uncategorized.
#[test]
fn counts_match_the_list_under_every_scope() {
    let mut s = Store::open_in_memory().unwrap();
    let f = feed(&mut s, "https://a.example/rss");
    let g = feed(&mut s, "https://b.example/rss");
    let h = feed(&mut s, "https://c.example/rss");
    ingest_unsorted(
        &mut s,
        f,
        &[entry("1", &[]), entry("2", &["Misc"]), entry("3", &["AI"]), entry("4", &[]), entry("5", &["AI"])],
    )
    .unwrap();
    ingest_unsorted(&mut s, g, &[entry("6", &[]), entry("7", &["Misc"]), entry("8", &[]), entry("9", &["AI", "Foot"])])
        .unwrap();
    ingest_unsorted(&mut s, h, &[entry("10", &["Foot"]), entry("11", &[])]).unwrap();
    let news = s.create_group("News").unwrap();
    let sports = s.create_group("Sports").unwrap();
    let sport = s.create_category("Sport").unwrap();
    let tech = s.create_category("Tech").unwrap();
    let local = s.create_category("Local").unwrap();
    let football = s.create_category("Football").unwrap();
    s.move_category(tech, Some(news), 0).unwrap();
    s.move_category(local, Some(news), 1).unwrap();
    s.move_category(football, Some(sports), 0).unwrap();
    map_term(&s, "AI", tech);
    map_term(&s, "Foot", football);
    s.set_feed_default_category(f, Some(sport)).unwrap();
    s.set_feed_default_category(h, Some(local)).unwrap();
    s.set_feed_group(g, Some(sports)).unwrap();
    s.set_feed_group(h, Some(news)).unwrap();
    s.set_article_category(article_id(&s, "4"), Some(local)).unwrap();
    for (guid, c) in [("5", local), ("6", local), ("7", tech), ("11", sport)] {
        set_ai(&s, article_id(&s, guid), c);
    }
    let folder = s.create_folder("Folder").unwrap();
    s.update_feed(g, "b", Some(folder)).unwrap();
    s.update_feed(h, "c", Some(folder)).unwrap();
    for guid in ["2", "6", "9", "10"] {
        s.set_read(article_id(&s, guid), true).unwrap();
    }
    for guid in ["3", "7", "11"] {
        s.set_saved(article_id(&s, guid), true).unwrap();
    }

    let scopes = [(None, None), (Some(f), None), (Some(g), None), (None, Some(folder)), (Some(h), Some(folder))];
    for view in [View::All, View::Unread, View::Saved] {
        for (feed_id, folder_id) in scopes {
            let filter = ListFilter { view, feed_id, folder_id };
            let counts = s.category_counts(filter).unwrap();
            let total = |q: SearchQuery| {
                let q = SearchQuery {
                    view,
                    feed_ids: feed_id.into_iter().collect(),
                    folder_ids: folder_id.into_iter().collect(),
                    ..q
                };
                s.search(&q, page()).unwrap().total
            };
            let at = format!("{view:?} feed {feed_id:?} folder {folder_id:?}");
            for c in s.categories().unwrap() {
                let listed = total(SearchQuery { category_ids: vec![c.id], ..Default::default() });
                assert_eq!(counts.categories.get(&c.id).copied().unwrap_or(0), listed, "{} at {at}", c.name);
            }
            for group in [news, sports] {
                let listed = total(SearchQuery { group_ids: vec![group], ..Default::default() });
                assert_eq!(counts.groups.get(&group).copied().unwrap_or(0), listed, "group {group} at {at}");
            }
            for group_feed in [g, h] {
                let listed = total(SearchQuery { category_feed_id: Some(group_feed), ..Default::default() });
                assert_eq!(counts.feeds.get(&group_feed).copied().unwrap_or(0), listed, "feed {group_feed} at {at}");
            }
            let listed = total(SearchQuery { uncategorized: true, ..Default::default() });
            assert_eq!(counts.uncategorized, listed, "uncategorized at {at}");
        }
    }
    // The fixture exercises every source: nothing above is vacuous.
    let all = s.category_counts(ListFilter::default()).unwrap();
    assert_eq!(all.categories.get(&sport), Some(&2), "f's default: 1 and 2");
    assert_eq!(all.categories.get(&local), Some(&3), "manual 4, AI 6, h's default 11");
    assert_eq!(all.categories.get(&tech), Some(&4), "mapped 3, 5 and 9; AI 7");
    assert_eq!(all.uncategorized, 1, "only article 8");
}

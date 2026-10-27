use juttusihti_core::store::Store;
use juttusihti_core::*;

fn store_with_article() -> (Store, ArticleId) {
    let mut s = Store::open_in_memory().unwrap();
    let f =
        s.add_feed(&NewFeed { url: "https://a.example/rss".into(), title: "A".into(), ..Default::default() }).unwrap();
    s.ingest(
        f,
        &[NewArticle {
            guid: "1".into(),
            url: Some("https://a.example/1".into()),
            title: "Et usko mitä tapahtui".into(),
            summary: Some("Helsingin raitiolinja 15 avataan maaliskuussa.".into()),
            content: None,
            author: None,
            published_at: Some(1_790_812_800),
        }],
    )
    .unwrap();
    let id = s.conn().query_row("SELECT id FROM articles", [], |r| r.get(0)).unwrap();
    (s, id)
}

#[test]
fn rewrites_are_suggestions_and_user_decisions_stick() {
    let (s, id) = store_with_article();
    assert!(s.shown_headlines(&[id]).unwrap().is_empty());

    s.suggest_headline(id, "Raitiolinja 15 avataan maaliskuussa", None, None).unwrap();
    assert_eq!(s.shown_headlines(&[id]).unwrap()[&id], "Raitiolinja 15 avataan maaliskuussa");
    // A later run may still replace an undecided rewrite.
    s.suggest_headline(id, "Helsingin raitiolinja 15 avataan maaliskuussa", None, None).unwrap();
    assert_eq!(s.headline(id).unwrap().unwrap().headline, "Helsingin raitiolinja 15 avataan maaliskuussa");

    s.decide_headline(id, Status::Rejected).unwrap();
    assert!(s.shown_headlines(&[id]).unwrap().is_empty(), "rejected rewrites are hidden");
    s.suggest_headline(id, "Something else", None, None).unwrap();
    let h = s.headline(id).unwrap().unwrap();
    assert_eq!((h.status, h.headline.as_str()), (Status::Rejected, "Helsingin raitiolinja 15 avataan maaliskuussa"));

    // The source title is untouched.
    assert_eq!(s.article(id).unwrap().title, "Et usko mitä tapahtui");
}

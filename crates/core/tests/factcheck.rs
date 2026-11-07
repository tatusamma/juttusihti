use juttusihti_core::factcheck::*;
use juttusihti_core::pricing::{Billing, Provider};
use juttusihti_core::store::Store;
use juttusihti_core::*;

fn entry(guid: &str) -> NewArticle {
    NewArticle {
        guid: guid.into(),
        url: Some(format!("https://news.example/{guid}")),
        title: format!("Title {guid}"),
        summary: None,
        content: None,
        author: None,
        published_at: Some(1_790_812_800),
    }
}

fn seeded() -> (Store, Vec<ArticleId>) {
    let mut s = Store::open_in_memory().unwrap();
    let feed = s
        .add_feed(&NewFeed { url: "https://fi.example/rss".into(), title: "Suomi".into(), ..Default::default() })
        .unwrap();
    s.ingest(feed, &[entry("1"), entry("2")]).unwrap();
    let ids = {
        let mut stmt = s.conn().prepare("SELECT id FROM articles ORDER BY id").unwrap();
        stmt.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
    };
    (s, ids)
}

fn claim(quote: &str, verdict: Verdict) -> ClaimResult {
    ClaimResult {
        quote: quote.into(),
        verdict,
        reason: "because".into(),
        sources: vec![Source {
            url: "https://other.example/a".into(),
            title: "Other".into(),
            kind: SourceKind::Web,
            publisher: None,
            rating: None,
        }],
    }
}

fn check(article_id: ArticleId, claims: Vec<ClaimResult>) -> FactCheck {
    FactCheck {
        article_id,
        checked_at: 1_790_900_000,
        provider: Provider::Anthropic,
        model: "some-model".into(),
        billing: Billing::ApiKey,
        prompt_version: FACT_CHECK_PROMPT_VERSION,
        language: "fi".into(),
        web_search: true,
        claims,
        guidelines: vec![GuidelineCheck {
            guideline: Guideline::Headline15,
            status: GuidelineStatus::Ok,
            reason: "fine".into(),
        }],
        cost: Some(12_345),
    }
}

fn row_count(s: &Store) -> i64 {
    s.conn().query_row("SELECT count(*) FROM article_fact_checks", [], |r| r.get(0)).unwrap()
}

#[test]
fn saves_and_reads_back_a_check() {
    let (s, ids) = seeded();
    let fc = check(ids[0], vec![claim("Väite", Verdict::Supported)]);
    assert_eq!(s.fact_check(ids[0]).unwrap(), None);
    s.save_fact_check(&fc).unwrap();
    assert_eq!(s.fact_check(ids[0]).unwrap(), Some(fc));
}

#[test]
fn check_again_replaces_the_row() {
    let (s, ids) = seeded();
    s.save_fact_check(&check(ids[0], vec![claim("Ensimmäinen", Verdict::Supported)])).unwrap();
    let second = check(ids[0], vec![claim("Toinen", Verdict::Contradicted), claim("Kolmas", Verdict::Unverified)]);
    s.save_fact_check(&second).unwrap();
    assert_eq!(s.fact_check(ids[0]).unwrap(), Some(second));
    assert_eq!(row_count(&s), 1);
}

#[test]
fn flags_only_articles_with_contradicted_or_misleading() {
    let (s, ids) = seeded();
    s.save_fact_check(&check(ids[0], vec![claim("a", Verdict::Contradicted)])).unwrap();
    s.save_fact_check(&check(ids[1], vec![claim("b", Verdict::Unverified)])).unwrap();
    let flags = s.fact_check_flags(&ids).unwrap();
    assert_eq!(flags, std::collections::HashSet::from([ids[0]]));
    assert!(s.fact_check_flags(&[]).unwrap().is_empty());
    assert!(s.fact_check_flags(&[ids[1]]).unwrap().is_empty());
}

#[test]
fn an_unreadable_row_reads_as_no_check() {
    let (s, ids) = seeded();
    s.save_fact_check(&check(ids[0], vec![])).unwrap();
    s.conn().execute("UPDATE article_fact_checks SET result_json = '{broken'", []).unwrap();
    assert_eq!(s.fact_check(ids[0]).unwrap(), None);
}

#[test]
fn deleting_the_article_removes_its_check() {
    let (s, ids) = seeded();
    s.save_fact_check(&check(ids[0], vec![claim("a", Verdict::Misleading)])).unwrap();
    s.conn().execute("DELETE FROM articles WHERE id = ?1", [ids[0]]).unwrap();
    assert_eq!(row_count(&s), 0);
}

#[test]
fn limit_defaults_to_one_dollar_and_can_be_changed() {
    let (s, _) = seeded();
    assert_eq!(s.fact_check_limit().unwrap(), 1_000_000);
    s.set_fact_check_limit(2_500_000).unwrap();
    assert_eq!(s.fact_check_limit().unwrap(), 2_500_000);
}

//! Full-article fetching against a scripted local HTTP server. No external
//! network: pages are local fixtures.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use juttusihti_core::extract::{Candidate, ExtractStatus};
use juttusihti_core::store::Store;
use juttusihti_core::{NewArticle, NewFeed, now};
use juttusihti_fetch::article::{ArticleFetcher, ArticlePolicy, fetch_full_text};
use juttusihti_fetch::{FetchError, Policy};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const ARTICLE: &str = include_str!("../../core/tests/fixtures/article_page.html");

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn html(body: &str) -> Self {
        Self { status: 200, headers: vec![("Content-Type", "text/html; charset=utf-8".into())], body: body.into() }
    }
    fn redirect(to: &str) -> Self {
        Self { status: 302, headers: vec![("Location", to.into())], body: vec![] }
    }
    fn with(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        Self { status, headers: vec![("Content-Type", content_type.into())], body }
    }
}

/// Serves `route(path)` for each request and records (path, time) pairs.
async fn serve(
    route: impl Fn(&str, &str) -> Reply + Send + Sync + 'static,
) -> (String, Arc<Mutex<Vec<(String, Instant)>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (log, route, origin) = (seen.clone(), Arc::new(route), base.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let (log, route, origin) = (log.clone(), route.clone(), origin.clone());
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let len = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..len]).to_string();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                log.lock().unwrap().push((path.clone(), Instant::now()));
                let reply = route(&origin, &path);
                let mut out = format!(
                    "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                    reply.status,
                    reply.body.len()
                );
                for (k, v) in &reply.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                let _ = sock.write_all(out.as_bytes()).await;
                let _ = sock.write_all(&reply.body).await;
            });
        }
    });
    (base, seen)
}

fn fetcher(interval: Duration) -> ArticleFetcher {
    let policy = Policy { timeout: Duration::from_secs(5), ..Default::default() };
    let article =
        ArticlePolicy { per_host_interval: interval, allow_hosts: vec!["127.0.0.1".into()], ..Default::default() };
    ArticleFetcher::new(&policy, article).unwrap()
}

fn paths(seen: &Mutex<Vec<(String, Instant)>>) -> Vec<String> {
    seen.lock().unwrap().iter().map(|(p, _)| p.clone()).collect()
}

#[tokio::test]
async fn teaser_entries_gain_the_article_text() {
    let (base, seen) = serve(|origin, path| match path {
        "/uutiset/1" => Reply::redirect(&format!("{origin}/uutiset/1/final")),
        "/uutiset/1/final" => Reply::html(ARTICLE),
        _ => Reply::with(404, "text/plain", vec![]),
    })
    .await;

    let mut store = Store::open_in_memory().unwrap();
    let feed = store.add_feed(&NewFeed { url: format!("{base}/rss"), ..Default::default() }).unwrap();
    let entry = |guid: &str, path: &str| NewArticle {
        guid: guid.into(),
        url: Some(format!("{base}{path}")),
        title: "Sataman laajennus viivästyy".into(),
        summary: Some("Laajennus myöhästyy. Lue lisää.".into()),
        content: None,
        author: None,
        published_at: Some(now() - 60),
    };
    store.ingest(feed, &[entry("1", "/uutiset/1"), entry("2", "/puuttuu")]).unwrap();
    store.set_feed_full_text(feed, true).unwrap();
    let candidates = store.full_text_candidates(feed, 10).unwrap();
    assert_eq!(candidates.len(), 2);

    let store = Mutex::new(store);
    let gained = fetch_full_text(&fetcher(Duration::ZERO), candidates, |id, url, result| {
        let r = store.lock().unwrap().save_extract(id, &url, result);
        async move { r }
    })
    .await;
    assert_eq!(gained, 1);

    let store = store.into_inner().unwrap();
    let id: i64 = store.conn().query_row("SELECT id FROM articles WHERE guid = '1'", [], |r| r.get(0)).unwrap();
    let extract = store.article_extract(id).unwrap().unwrap();
    assert_eq!(extract.status, ExtractStatus::Ok);
    assert_eq!(extract.url, format!("{base}/uutiset/1/final"), "the page actually read is recorded");
    let body = store.article_text(id).unwrap();
    assert!(body.contains("vuoteen 2028") && !body.contains("Tilaa uutiskirje"), "{body}");

    let missing: i64 = store.conn().query_row("SELECT id FROM articles WHERE guid = '2'", [], |r| r.get(0)).unwrap();
    let failed = store.article_extract(missing).unwrap().unwrap();
    assert_eq!(failed.status, ExtractStatus::Failed);
    assert!(failed.error.unwrap().contains("404"));
    // Both are recorded, so the next refresh does not fetch them again.
    assert!(store.full_text_candidates(feed, 10).unwrap().is_empty());
    let mut requested = paths(&seen);
    requested.sort();
    assert_eq!(requested, ["/puuttuu", "/uutiset/1", "/uutiset/1/final"]);
}

#[tokio::test]
async fn redirects_to_private_or_non_web_targets_are_refused() {
    let (base, seen) = serve(|origin, path| match path {
        "/to-localhost" => Reply::redirect(&origin.replace("127.0.0.1", "localhost")),
        "/to-metadata" => Reply::redirect("http://169.254.169.254/latest/meta-data/"),
        "/to-v6-loopback" => Reply::redirect("http://[::1]/"),
        "/to-file" => Reply::redirect("file:///etc/passwd"),
        "/loop" => Reply::redirect("/loop"),
        "/relative" => Reply::redirect("final?x=1"),
        "/final?x=1" => Reply::html("<p>ok</p>"),
        _ => Reply::with(404, "text/plain", vec![]),
    })
    .await;
    let f = fetcher(Duration::ZERO);
    for path in ["/to-localhost", "/to-metadata", "/to-v6-loopback"] {
        let r = f.fetch_page(&format!("{base}{path}")).await;
        assert!(matches!(r, Err(FetchError::Blocked(_))), "{path}: {r:?}");
    }
    assert!(matches!(f.fetch_page(&format!("{base}/to-file")).await, Err(FetchError::UnsupportedUrl)));
    let r = f.fetch_page(&format!("{base}/loop")).await;
    assert!(matches!(&r, Err(FetchError::Blocked(m)) if m.contains("redirects")), "{r:?}");
    // 1 request + 5 redirects followed.
    assert_eq!(paths(&seen).iter().filter(|p| *p == "/loop").count(), 6);
    let page = f.fetch_page(&format!("{base}/relative")).await.unwrap();
    assert_eq!(page.final_url, format!("{base}/final?x=1"));

    // Without the test exemption the local server itself is never contacted.
    let strict = ArticleFetcher::new(&Policy::default(), ArticlePolicy::default()).unwrap();
    let before = paths(&seen).len();
    assert!(matches!(strict.fetch_page(&format!("{base}/relative")).await, Err(FetchError::Blocked(_))));
    assert_eq!(paths(&seen).len(), before);
}

#[tokio::test]
async fn size_and_type_limits_hold() {
    let (base, _) = serve(|_, path| match path {
        "/big" => Reply::html(&"<p>sana</p>".repeat(2_000)),
        "/pdf" => Reply::with(200, "application/pdf", b"%PDF-1.7".to_vec()),
        "/latin1" => Reply::with(200, "text/html; charset=ISO-8859-1", b"<p>P\xe4iv\xe4n s\xe4\xe4</p>".to_vec()),
        _ => Reply::with(404, "text/plain", vec![]),
    })
    .await;
    let policy = Policy { max_bytes: 10_000, ..Default::default() };
    let article = ArticlePolicy {
        allow_hosts: vec!["127.0.0.1".into()],
        per_host_interval: Duration::ZERO,
        ..Default::default()
    };
    let f = ArticleFetcher::new(&policy, article).unwrap();
    assert!(
        matches!(f.fetch_page(&format!("{base}/big")).await, Err(FetchError::TooLarge(10_000))),
        "the feed size limit caps article pages too"
    );
    assert!(
        matches!(f.fetch_page(&format!("{base}/pdf")).await, Err(FetchError::NotHtml(m)) if m == "application/pdf")
    );
    assert_eq!(f.fetch_page(&format!("{base}/latin1")).await.unwrap().html, "<p>Päivän sää</p>");
}

#[tokio::test]
async fn requests_to_one_host_are_spaced_out() {
    let (base, seen) = serve(|_, _| Reply::html("<p>ok</p>")).await;
    let interval = Duration::from_millis(300);
    let f = fetcher(interval);
    // Concurrent callers share the limiter.
    let (g, ua, ub, uc) = (f.clone(), format!("{base}/a"), format!("{base}/b"), format!("{base}/c"));
    let (a, b, c) = tokio::join!(f.fetch_page(&ua), f.fetch_page(&ub), g.fetch_page(&uc));
    assert!(a.is_ok() && b.is_ok() && c.is_ok());
    let mut times: Vec<Instant> = seen.lock().unwrap().iter().map(|(_, t)| *t).collect();
    times.sort();
    for pair in times.windows(2) {
        // Small slack for timer granularity.
        assert!(pair[1] - pair[0] >= interval - Duration::from_millis(20), "{:?}", pair[1] - pair[0]);
    }
}

#[tokio::test]
async fn candidates_without_text_are_recorded_as_empty() {
    let (base, _) = serve(|_, _| Reply::html("<html><body><p>Hyväksy evästeet jatkaaksesi.</p></body></html>")).await;
    let f = fetcher(Duration::ZERO);
    let (url, result) = f.extract(&Candidate { article_id: 1, url: format!("{base}/cookie-wall") }).await;
    assert_eq!(url, format!("{base}/cookie-wall"));
    assert_eq!(result, juttusihti_core::extract::Extraction::NoText);
}

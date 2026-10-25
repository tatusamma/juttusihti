//! Fetcher tests against a scripted local HTTP server. No external network.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use juttusihti_core::NewFeed;
use juttusihti_core::store::Store;
use juttusihti_fetch::{FetchError, FetchOutcome, Fetcher, Policy, apply};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const RSS: &str = include_str!("../../core/tests/fixtures/yle_fi.rss");

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
    delay: Duration,
}

impl Reply {
    fn ok(body: &str) -> Self {
        Self { status: 200, headers: vec![], body: body.as_bytes().to_vec(), delay: Duration::ZERO }
    }
    fn status(status: u16) -> Self {
        Self { status, headers: vec![], body: vec![], delay: Duration::ZERO }
    }
    fn header(mut self, k: &'static str, v: &str) -> Self {
        self.headers.push((k, v.to_string()));
        self
    }
}

/// Serves scripted replies in order (the last one repeats) and records
/// request heads.
async fn serve(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        let mut n = 0;
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let reply = replies[n.min(replies.len() - 1)].clone();
            n += 1;
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let len = sock.read(&mut buf).await.unwrap_or(0);
                log.lock().unwrap().push(String::from_utf8_lossy(&buf[..len]).to_string());
                tokio::time::sleep(reply.delay).await;
                let mut head = format!(
                    "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                    reply.status,
                    reply.body.len()
                );
                for (k, v) in &reply.headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                head.push_str("\r\n");
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&reply.body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}"), seen)
}

fn fast_policy() -> Policy {
    Policy {
        timeout: Duration::from_millis(500),
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(50),
        ..Default::default()
    }
}

#[tokio::test]
async fn conditional_requests_round_trip_into_store() {
    let (base, seen) = serve(vec![
        Reply::ok(RSS).header("ETag", "\"v1\"").header("Last-Modified", "Mon, 05 Oct 2026 06:00:00 GMT"),
        Reply::status(304),
    ])
    .await;
    let url = format!("{base}/rss");
    let fetcher = Fetcher::new(fast_policy()).unwrap();
    let mut store = Store::open_in_memory().unwrap();
    let feed_id = store.add_feed(&NewFeed { url: url.clone(), ..Default::default() }).unwrap();

    let stats = apply(&mut store, feed_id, fetcher.fetch(&url, None, None).await).unwrap();
    assert_eq!(stats.inserted, 3);
    let feed = store.feed(feed_id).unwrap();
    assert_eq!(feed.etag.as_deref(), Some("\"v1\""));
    assert_eq!(feed.title, "Uutiset – Esimerkki", "title adopted from the document");

    let second = fetcher.fetch(&url, feed.etag.as_deref(), feed.last_modified.as_deref()).await.unwrap();
    assert_eq!(second, FetchOutcome::NotModified);
    let req = seen.lock().unwrap()[1].to_lowercase();
    assert!(req.contains("if-none-match: \"v1\""), "{req}");
    assert!(req.contains("if-modified-since: mon, 05 oct 2026 06:00:00 gmt"), "{req}");
    assert!(req.contains("user-agent: juttusihti/"), "{req}");
}

#[tokio::test]
async fn retries_server_errors_then_succeeds() {
    let (base, seen) = serve(vec![Reply::status(503), Reply::status(500), Reply::ok(RSS)]).await;
    let fetcher = Fetcher::new(fast_policy()).unwrap();
    let out = fetcher.fetch(&format!("{base}/rss"), None, None).await.unwrap();
    assert!(matches!(out, FetchOutcome::Fetched { .. }));
    assert_eq!(seen.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn gives_up_after_bounded_attempts() {
    let (base, seen) = serve(vec![Reply::status(502)]).await;
    let fetcher = Fetcher::new(fast_policy()).unwrap();
    let err = fetcher.fetch(&format!("{base}/rss"), None, None).await.unwrap_err();
    assert!(matches!(err, FetchError::Status(s) if s.as_u16() == 502));
    assert_eq!(seen.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn client_errors_are_not_retried_and_are_recorded() {
    let (base, seen) = serve(vec![Reply::status(404)]).await;
    let url = format!("{base}/missing");
    let fetcher = Fetcher::new(fast_policy()).unwrap();
    let mut store = Store::open_in_memory().unwrap();
    let feed_id = store.add_feed(&NewFeed { url: url.clone(), ..Default::default() }).unwrap();
    assert!(apply(&mut store, feed_id, fetcher.fetch(&url, None, None).await).is_err());
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert!(store.feed(feed_id).unwrap().last_error.unwrap().contains("404"));
}

#[tokio::test]
async fn times_out() {
    let mut slow = Reply::ok(RSS);
    slow.delay = Duration::from_secs(5);
    let (base, _) = serve(vec![slow]).await;
    let policy = Policy { max_attempts: 1, ..fast_policy() };
    let err = Fetcher::new(policy).unwrap().fetch(&format!("{base}/rss"), None, None).await.unwrap_err();
    assert!(matches!(err, FetchError::Http(ref e) if e.is_timeout()), "{err}");
}

#[tokio::test]
async fn rejects_oversized_bodies() {
    let (base, _) = serve(vec![Reply::ok(&"x".repeat(2048))]).await;
    let policy = Policy { max_bytes: 1024, ..fast_policy() };
    let err = Fetcher::new(policy).unwrap().fetch(&format!("{base}/big"), None, None).await.unwrap_err();
    assert!(matches!(err, FetchError::TooLarge(1024)));
}

#[tokio::test]
async fn discovers_feeds_from_html_and_direct_urls() {
    let html =
        r#"<html><head><link rel="alternate" type="application/rss+xml" href="/rss" title="Main"></head></html>"#;
    let (base, _) = serve(vec![Reply::ok(html), Reply::ok(RSS)]).await;
    let fetcher = Fetcher::new(fast_policy()).unwrap();
    let found = fetcher.discover(&format!("{base}/")).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].url, format!("{base}/rss"));
    let direct = fetcher.discover(&format!("{base}/rss")).await.unwrap();
    assert_eq!(direct[0].title.as_deref(), Some("Uutiset – Esimerkki"));
}

#[tokio::test]
async fn refuses_non_web_urls() {
    let fetcher = Fetcher::new(fast_policy()).unwrap();
    assert!(matches!(fetcher.fetch("file:///etc/passwd", None, None).await, Err(FetchError::UnsupportedUrl)));
}

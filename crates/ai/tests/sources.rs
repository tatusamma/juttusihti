//! Evidence-source lookups against a local server; no real service is contacted.

use std::sync::{Arc, Mutex};

use juttusihti_ai::sources::Sources;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const KEY: &str = "secret-google-key-777";

/// Serves `replies` in order (the last one repeats) and records each request head.
async fn serve(replies: Vec<(u16, &'static str)>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let mut buf = Vec::new();
            let mut tmp = [0u8; 8192];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match sock.read(&mut tmp).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                }
            }
            let idx = {
                let mut l = log.lock().unwrap();
                l.push(String::from_utf8_lossy(&buf).to_string());
                l.len() - 1
            };
            let (status, body) = replies[idx.min(replies.len() - 1)];
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (base, seen)
}

const ONE_CLAIM: &str = r#"{"claims":[{"text":"x","claimReview":[{"publisher":{"name":"P"},"url":"https://p.example/a","title":"T","textualRating":"False"}]}]}"#;

#[tokio::test]
async fn retries_without_language_when_nothing_found() {
    let (base, seen) = serve(vec![(200, "{}"), (200, ONE_CLAIM)]).await;
    let s = Sources::with_bases(Some(KEY.into()), &base, &base).unwrap();
    let found = s.fact_checks("moon landing", "fi").await.unwrap();
    assert_eq!(found.len(), 1);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].contains("languageCode=fi"));
    assert!(!seen[1].contains("languageCode"));
    assert!(seen[0].contains("pageSize=5") && seen[0].contains("moon+landing") | seen[0].contains("moon%20landing"));
}

#[tokio::test]
async fn no_key_sends_nothing() {
    let (base, seen) = serve(vec![(200, "{}")]).await;
    let s = Sources::with_bases(None, &base, &base).unwrap();
    assert!(!s.has_google());
    assert!(s.fact_checks("q", "en").await.unwrap().is_empty());
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn errors_do_not_contain_the_key() {
    let (base, _) = serve(vec![(403, r#"{"error":"nope"}"#)]).await;
    let s = Sources::with_bases(Some(KEY.into()), &base, &base).unwrap();
    let err = s.fact_checks("q", "en").await.unwrap_err();
    assert!(!err.contains(KEY), "{err}");
    // A connection failure must not leak the key either.
    let dead = Sources::with_bases(Some(KEY.into()), "http://127.0.0.1:1", &base).unwrap();
    let err = dead.fact_checks("q", "en").await.unwrap_err();
    assert!(!err.contains(KEY), "{err}");
}

#[tokio::test]
async fn sends_generic_user_agent() {
    let (base, seen) =
        serve(vec![(200, r#"{"pages":[{"key":"A b","title":"A b","excerpt":"<span>x</span>"}]}"#)]).await;
    let s = Sources::with_bases(None, &base, &base).unwrap();
    let pages = s.wikipedia("a b", "en").await.unwrap();
    assert_eq!(pages[0].url, format!("{base}/wiki/A%20b"));
    let head = seen.lock().unwrap()[0].to_ascii_lowercase();
    let expected =
        format!("user-agent: juttusihti/{} (https://github.com/tero-k/juttusihti)", env!("CARGO_PKG_VERSION"));
    assert!(head.contains(&expected), "{head}");
    assert!(head.contains("/w/rest.php/v1/search/page?q=a+b&limit=2") || head.contains("q=a%20b&limit=2"));
}

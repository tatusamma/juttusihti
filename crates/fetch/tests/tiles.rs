//! Tile client behaviour required by the OSM tile usage policy, tested
//! against a local scripted server.

use std::sync::{Arc, Mutex};

use juttusihti_fetch::tiles::{TileCache, TileClient, TileError, TileId};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake";

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
}

fn reply(status: u16, headers: &[(&'static str, &str)], body: &[u8]) -> Reply {
    Reply { status, headers: headers.iter().map(|(k, v)| (*k, v.to_string())).collect(), body: body.to_vec() }
}

async fn serve(replies: Vec<Reply>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        let mut n = 0;
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let r = replies[n.min(replies.len() - 1)].clone();
            n += 1;
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let len = sock.read(&mut buf).await.unwrap_or(0);
                log.lock().unwrap().push(String::from_utf8_lossy(&buf[..len]).to_lowercase());
                let mut head =
                    format!("HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n", r.status, r.body.len());
                for (k, v) in &r.headers {
                    head.push_str(&format!("{k}: {v}\r\n"));
                }
                head.push_str("\r\n");
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&r.body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("{base}/{{z}}/{{x}}/{{y}}.png"), seen)
}

const T: TileId = TileId { z: 5, x: 17, y: 9 };

#[tokio::test]
async fn fresh_tiles_come_from_cache_without_requests() {
    let dir = tempfile::tempdir().unwrap();
    let (tpl, seen) =
        serve(vec![reply(200, &[("Content-Type", "image/png"), ("Cache-Control", "max-age=3600")], PNG)]).await;
    let c = TileClient::new_unchecked(&tpl, TileCache::new(dir.path()));
    assert_eq!(c.tile(T).await.unwrap(), PNG);
    assert_eq!(c.tile(T).await.unwrap(), PNG);
    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 1, "second view served from cache");
    assert!(reqs[0].starts_with("get /5/17/9.png"));
    assert!(reqs[0].contains("user-agent: juttusihti/"), "distinct app user agent: {}", reqs[0]);
    assert!(!reqs[0].contains("no-cache"), "never sends no-cache");
}

#[tokio::test]
async fn expired_tiles_are_revalidated_conditionally() {
    let dir = tempfile::tempdir().unwrap();
    let (tpl, seen) = serve(vec![
        reply(200, &[("Content-Type", "image/png"), ("Cache-Control", "max-age=0"), ("ETag", "\"abc\"")], PNG),
        reply(304, &[("Cache-Control", "max-age=3600")], b""),
    ])
    .await;
    let c = TileClient::new_unchecked(&tpl, TileCache::new(dir.path()));
    c.tile(T).await.unwrap();
    assert_eq!(c.tile(T).await.unwrap(), PNG, "304 keeps the cached image");
    assert_eq!(c.tile(T).await.unwrap(), PNG);
    let reqs = seen.lock().unwrap().clone();
    assert_eq!(reqs.len(), 2, "after the 304 the tile is fresh again");
    assert!(reqs[1].contains("if-none-match: \"abc\""), "{}", reqs[1]);
}

#[tokio::test]
async fn tiles_without_cache_headers_are_kept_for_a_week() {
    let dir = tempfile::tempdir().unwrap();
    let (tpl, seen) = serve(vec![reply(200, &[("Content-Type", "image/png")], PNG)]).await;
    let cache = TileCache::new(dir.path());
    let c = TileClient::new_unchecked(&tpl, cache.clone());
    c.tile(T).await.unwrap();
    let (_, meta) = cache.read(T).unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    assert!(meta.fresh_until >= now + 7 * 24 * 3600 - 60);
    c.tile(T).await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn no_store_is_honoured() {
    let dir = tempfile::tempdir().unwrap();
    let (tpl, seen) =
        serve(vec![reply(200, &[("Content-Type", "image/png"), ("Cache-Control", "no-store")], PNG)]).await;
    let cache = TileCache::new(dir.path());
    let c = TileClient::new_unchecked(&tpl, cache.clone());
    c.tile(T).await.unwrap();
    assert!(cache.read(T).is_none());
    c.tile(T).await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn rejects_non_images_and_oversized_tiles() {
    let dir = tempfile::tempdir().unwrap();
    let (tpl, _) = serve(vec![reply(200, &[("Content-Type", "text/html")], b"<html>blocked</html>")]).await;
    let c = TileClient::new_unchecked(&tpl, TileCache::new(dir.path()));
    assert_eq!(c.tile(T).await.unwrap_err(), TileError::NotImage);

    let big = vec![0u8; 2 * 1024 * 1024];
    let (tpl, _) = serve(vec![reply(200, &[("Content-Type", "image/png")], &big)]).await;
    let c = TileClient::new_unchecked(&tpl, TileCache::new(dir.path()));
    assert_eq!(c.tile(T).await.unwrap_err(), TileError::TooLarge);
}

#[tokio::test]
async fn stale_cache_is_used_when_offline_or_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let cache = TileCache::new(dir.path());
    let (tpl, _) = serve(vec![
        reply(200, &[("Content-Type", "image/png"), ("Cache-Control", "max-age=0")], PNG),
        reply(429, &[], b""),
    ])
    .await;
    let c = TileClient::new_unchecked(&tpl, cache.clone());
    c.tile(T).await.unwrap();
    assert_eq!(c.tile(T).await.unwrap(), PNG, "throttled: fall back to the cached tile");
    let offline = TileClient::new_unchecked("http://127.0.0.1:9/{z}/{x}/{y}.png", cache.clone());
    assert_eq!(offline.tile(T).await.unwrap(), PNG);
    assert!(offline.tile(TileId { z: 5, x: 1, y: 1 }).await.is_err(), "uncached tile offline is an error");
    assert!(cache.size() > 0);
    cache.clear().unwrap();
    assert_eq!(cache.size(), 0);
}

#[test]
fn production_client_requires_https_template() {
    let dir = tempfile::tempdir().unwrap();
    assert!(TileClient::new("http://insecure/{z}/{x}/{y}.png", TileCache::new(dir.path())).is_err());
    assert!(TileClient::new(juttusihti_fetch::tiles::OSM_TILE_URL, TileCache::new(dir.path())).is_ok());
}

//! A scripted local HTTP server shared by the integration tests. It records
//! every request and answers with whatever the test's handler returns.

#![allow(dead_code)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone)]
pub struct Recorded {
    pub path: String,
    pub headers: String,
    pub body: Value,
    /// The body as text (form-encoded OAuth requests are not JSON).
    pub raw: String,
}

#[derive(Clone)]
pub struct MockReply {
    pub status: u16,
    pub body: Value,
    pub delay: Duration,
}

pub type Handler = Arc<dyn Fn(&Recorded, usize) -> MockReply + Send + Sync>;

pub struct Mock {
    pub base: String,
    pub seen: Arc<Mutex<Vec<Recorded>>>,
}

impl Mock {
    pub fn count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
    pub fn requests(&self) -> Vec<Recorded> {
        self.seen.lock().unwrap().clone()
    }
}

async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 16384];
    loop {
        let n = sock.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_string();
            let len: usize = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))
                })
                .unwrap_or(0);
            while buf.len() < pos + 4 + len {
                let n = sock.read(&mut tmp).await.ok()?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let path = head.lines().next()?.split_whitespace().nth(1)?.to_string();
            return Some((path, head, buf[pos + 4..].to_vec()));
        }
    }
}

pub async fn mock(handler: Handler) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        let counter = Arc::new(AtomicUsize::new(0));
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let (log, handler, counter) = (log.clone(), handler.clone(), counter.clone());
            tokio::spawn(async move {
                let Some((path, headers, body)) = read_request(&mut sock).await else { return };
                let rec = Recorded {
                    path,
                    headers,
                    body: serde_json::from_slice(&body).unwrap_or(Value::Null),
                    raw: String::from_utf8_lossy(&body).into_owned(),
                };
                let n = counter.fetch_add(1, Ordering::SeqCst);
                log.lock().unwrap().push(rec.clone());
                let reply = handler(&rec, n);
                tokio::time::sleep(reply.delay).await;
                // A string body is sent as is: a server-sent event stream.
                let (body, kind) = match &reply.body {
                    Value::String(sse) => (sse.clone(), "text/event-stream"),
                    other => (other.to_string(), "application/json"),
                };
                let head = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    reply.status,
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Mock { base, seen }
}

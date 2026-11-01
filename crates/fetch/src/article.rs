//! Fetching article pages for full-article text.
//!
//! Article URLs come from feeds, so they are untrusted. Compared with feed
//! fetching, this adds:
//! - every hop of a redirect chain is checked: only http(s), and no
//!   loopback, private, link-local or otherwise non-public addresses (by IP
//!   literal, by host name such as `localhost`, and by resolving the name);
//! - a smaller response cap ([`MAX_PAGE_BYTES`], never above the feed cap);
//! - at most one request per host every [`ArticlePolicy::per_host_interval`];
//! - no retries and no JavaScript: one plain GET per hop.
//!
//! Timeouts and the system proxy settings are the same as for feeds. With a
//! proxy, a host name the local resolver cannot resolve is left to the
//! proxy; literal and name-based checks still apply.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use juttusihti_core::extract::{self, Candidate, ExtractStatus, Extraction, MAX_PAGE_BYTES};
use reqwest::header::{ACCEPT, CONTENT_TYPE, LOCATION};
use reqwest::{StatusCode, redirect};
use tokio::time::Instant;
use url::{Host, Url};

use crate::{FetchError, Policy, USER_AGENT};

/// Limits specific to article pages; timeouts come from [`Policy`].
#[derive(Debug, Clone)]
pub struct ArticlePolicy {
    /// Response cap. The smaller of this and [`Policy::max_bytes`] applies.
    pub max_bytes: usize,
    /// Least time between two requests to the same host.
    pub per_host_interval: Duration,
    pub max_redirects: usize,
    /// Hosts (as written in the URL) exempt from the public-address check.
    /// Only for tests against a local server; empty in the app.
    pub allow_hosts: Vec<String>,
}

impl Default for ArticlePolicy {
    fn default() -> Self {
        Self {
            max_bytes: MAX_PAGE_BYTES,
            per_host_interval: Duration::from_secs(2),
            max_redirects: 5,
            allow_hosts: Vec::new(),
        }
    }
}

/// An HTML page as read, decoded to text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub final_url: String,
    pub html: String,
}

/// Fetches article pages. Cheap to clone; clones share the per-host limiter.
#[derive(Clone)]
pub struct ArticleFetcher {
    client: reqwest::Client,
    policy: ArticlePolicy,
    max_bytes: usize,
    next_slot: Arc<Mutex<HashMap<String, Instant>>>,
}

impl ArticleFetcher {
    pub fn new(policy: &Policy, article: ArticlePolicy) -> Result<Self, FetchError> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(policy.timeout)
            .connect_timeout(policy.connect_timeout)
            // Redirects are followed by hand so every hop is checked.
            .redirect(redirect::Policy::none())
            .build()?;
        let max_bytes = article.max_bytes.min(policy.max_bytes);
        Ok(Self { client, policy: article, max_bytes, next_slot: Arc::default() })
    }

    /// Fetches an HTML page, following up to `max_redirects` checked redirects.
    pub async fn fetch_page(&self, url: &str) -> Result<Page, FetchError> {
        let mut url = Url::parse(url).map_err(|_| FetchError::UnsupportedUrl)?;
        for _ in 0..=self.policy.max_redirects {
            check_target(&url, &self.policy.allow_hosts).await?;
            self.wait_for_host(&url).await;
            let mut resp = self
                .client
                .get(url.clone())
                .header(ACCEPT, "text/html, application/xhtml+xml;q=0.9, */*;q=0.1")
                .send()
                .await?;
            let status = resp.status();
            if status.is_redirection() && status != StatusCode::NOT_MODIFIED {
                let location =
                    resp.headers().get(LOCATION).and_then(|v| v.to_str().ok()).ok_or(FetchError::Status(status))?;
                url = url.join(location).map_err(|_| FetchError::UnsupportedUrl)?;
                continue;
            }
            if !status.is_success() {
                return Err(FetchError::Status(status));
            }
            let content_type =
                resp.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
            let mime = content_type.split(';').next().unwrap_or("").trim();
            if !(mime.is_empty() || mime == "text/html" || mime == "application/xhtml+xml") {
                return Err(FetchError::NotHtml(mime.to_string()));
            }
            if resp.content_length().is_some_and(|n| n as usize > self.max_bytes) {
                return Err(FetchError::TooLarge(self.max_bytes));
            }
            let mut body = Vec::new();
            while let Some(chunk) = resp.chunk().await? {
                if body.len() + chunk.len() > self.max_bytes {
                    return Err(FetchError::TooLarge(self.max_bytes));
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(Page { final_url: url.to_string(), html: decode(&body, &content_type) });
        }
        Err(FetchError::Blocked("too many redirects".into()))
    }

    /// Waits until this host's next request slot and reserves the one after.
    async fn wait_for_host(&self, url: &Url) {
        let key = url.host_str().unwrap_or("").to_ascii_lowercase();
        let slot = {
            let mut slots = self.next_slot.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            let slot = slots.get(&key).copied().filter(|t| *t > now).unwrap_or(now);
            slots.insert(key, slot + self.policy.per_host_interval);
            slot
        };
        tokio::time::sleep_until(slot).await;
    }

    /// Fetches one candidate's page and extracts its main text.
    pub async fn extract(&self, candidate: &Candidate) -> (String, Extraction) {
        match self.fetch_page(&candidate.url).await {
            Ok(page) => {
                let Page { final_url, html } = page;
                let url = final_url.clone();
                // Parsing a few MB of HTML is CPU work; keep it off the async workers.
                let text =
                    tokio::task::spawn_blocking(move || extract::extract_main_text(&html, &url)).await.ok().flatten();
                (final_url, text.map_or(Extraction::NoText, Extraction::Text))
            }
            Err(e) => (candidate.url.clone(), Extraction::Failed(e.to_string())),
        }
    }
}

/// Rejects URLs that are not http(s) or that point at non-public addresses.
/// Hosts listed in `allow_hosts` skip the address check.
pub async fn check_target(url: &Url, allow_hosts: &[String]) -> Result<(), FetchError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FetchError::UnsupportedUrl);
    }
    if url.host_str().is_some_and(|h| allow_hosts.iter().any(|a| a == h)) {
        return Ok(());
    }
    let blocked = |what: &str| Err(FetchError::Blocked(format!("{what} is not a public address")));
    match url.host() {
        None => Err(FetchError::UnsupportedUrl),
        Some(Host::Ipv4(ip)) if !is_public(IpAddr::V4(ip)) => blocked(&ip.to_string()),
        Some(Host::Ipv6(ip)) if !is_public(IpAddr::V6(ip)) => blocked(&ip.to_string()),
        Some(Host::Ipv4(_) | Host::Ipv6(_)) => Ok(()),
        Some(Host::Domain(name)) => {
            let name = name.trim_end_matches('.').to_ascii_lowercase();
            if name == "localhost"
                || [".localhost", ".local", ".internal", ".home.arpa"].iter().any(|s| name.ends_with(s))
                || !name.contains('.')
            {
                return blocked(&name);
            }
            let port = url.port_or_known_default().unwrap_or(80);
            // Unresolvable here may still be resolvable by a proxy; the
            // request itself fails if it is not.
            if let Ok(addrs) = tokio::net::lookup_host((name.as_str(), port)).await {
                for addr in addrs {
                    if !is_public(addr.ip()) {
                        return blocked(&format!("{name} ({})", addr.ip()));
                    }
                }
            }
            Ok(())
        }
    }
}

/// Whether an address is globally routable (not loopback, private,
/// link-local, shared, documentation, multicast, reserved, ...).
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public_v4(v4);
            }
            let s = v6.segments();
            let nat64 = s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0];
            if nat64 || s[..6] == [0, 0, 0, 0, 0, 0] {
                // NAT64 and IPv4-compatible forms embed an IPv4 address.
                return is_public_v4(Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8));
            }
            !(v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local fc00::/7
                || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (s[0] & 0xffc0) == 0xfec0 // site-local fec0::/10 (deprecated)
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
                || v6 == Ipv6Addr::new(0x100, 0, 0, 0, 0, 0, 0, 0))
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || o[0] == 0 // "this network"
        || (o[0] == 100 && (o[1] & 0xc0) == 64) // shared address space 100.64/10
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
        || (o[0] == 198 && (o[1] & 0xfe) == 18) // benchmarking 198.18/15
        || o[0] >= 240) // reserved
}

/// Decodes a page using the charset from the Content-Type header, else a
/// `<meta charset>` near the top of the page, else UTF-8.
fn decode(body: &[u8], content_type: &str) -> String {
    let label = charset_param(content_type).or_else(|| meta_charset(body));
    let encoding = label.and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes())).unwrap_or(encoding_rs::UTF_8);
    encoding.decode(body).0.into_owned()
}

fn charset_param(content_type: &str) -> Option<String> {
    content_type.split(';').skip(1).find_map(|p| {
        let (k, v) = p.split_once('=')?;
        (k.trim() == "charset").then(|| v.trim().trim_matches('"').to_string())
    })
}

fn meta_charset(body: &[u8]) -> Option<String> {
    let head = String::from_utf8_lossy(&body[..body.len().min(2048)]).to_ascii_lowercase();
    let at = head.find("charset=")? + "charset=".len();
    let value: String = head[at..]
        .trim_start_matches(['"', '\''])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'))
        .collect();
    (!value.is_empty()).then_some(value)
}

/// Fetches full-article text for teaser entries, one page at a time, and
/// hands each result to `save` as soon as it is read. Returns the number of
/// articles that gained text.
///
/// `save` stores one result; the app passes its database worker, tests a
/// plain [`Store`](juttusihti_core::store::Store).
pub async fn fetch_full_text<S, F, E>(fetcher: &ArticleFetcher, candidates: Vec<Candidate>, mut save: S) -> usize
where
    S: FnMut(i64, String, Extraction) -> F,
    F: std::future::Future<Output = Result<ExtractStatus, E>>,
    E: std::fmt::Display,
{
    let mut gained = 0;
    for c in candidates {
        let (url, result) = fetcher.extract(&c).await;
        if let Extraction::Failed(e) = &result {
            tracing::debug!(url = %c.url, error = %e, "article fetch failed");
        }
        match save(c.article_id, url, result).await {
            Ok(ExtractStatus::Ok) => gained += 1,
            Ok(_) => {}
            Err(e) => tracing::warn!(article = c.article_id, error = %e, "storing article text failed"),
        }
    }
    gained
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_public_addresses_pass() {
        for public in ["93.184.215.14", "1.1.1.1", "2606:4700::1111", "::ffff:93.184.215.14"] {
            assert!(is_public(ip(public)), "{public}");
        }
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "198.18.0.1",
            "240.0.0.1",
            "::1",
            "::",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "64:ff9b::a00:1",
            "::127.0.0.1",
        ] {
            assert!(!is_public(ip(private)), "{private}");
        }
    }

    #[tokio::test]
    async fn targets_are_checked_by_scheme_literal_and_name() {
        let check = |u: &str| {
            let u = Url::parse(u).unwrap();
            async move { check_target(&u, &[]).await }
        };
        assert!(matches!(check("file:///etc/passwd").await, Err(FetchError::UnsupportedUrl)));
        assert!(matches!(check("ftp://news.example/a").await, Err(FetchError::UnsupportedUrl)));
        for blocked in [
            "http://127.0.0.1/",
            "http://[::1]:8080/",
            "http://169.254.169.254/latest/meta-data/",
            "http://2130706433/",
            "http://0x7f.1/",
            "http://localhost/",
            "http://LOCALHOST./",
            "http://api.localhost/",
            "http://printer.local/",
            "http://intranet/",
        ] {
            assert!(matches!(check(blocked).await, Err(FetchError::Blocked(_))), "{blocked}");
        }
        assert!(check("https://93.184.215.14/").await.is_ok());
        let local = Url::parse("http://127.0.0.1/").unwrap();
        assert!(check_target(&local, &["127.0.0.1".into()]).await.is_ok(), "tests may allow their server");
        let other = Url::parse("http://127.0.0.2/").unwrap();
        assert!(check_target(&other, &["127.0.0.1".into()]).await.is_err(), "only the listed host");
    }

    #[test]
    fn decodes_declared_charsets() {
        let latin1 = b"<p>Hyv\xe4\xe4 p\xe4iv\xe4\xe4</p>";
        assert_eq!(decode(latin1, "text/html; charset=ISO-8859-1"), "<p>Hyvää päivää</p>");
        let meta = b"<html><head><meta charset=\"windows-1252\"></head><p>\xe4</p>";
        assert!(decode(meta, "text/html").contains("<p>ä</p>"));
        assert_eq!(decode("<p>sää</p>".as_bytes(), ""), "<p>sää</p>");
    }
}

//! HTTP fetching for feeds: conditional requests (ETag / Last-Modified),
//! timeouts, bounded retries with backoff, response size limits and
//! website feed discovery.
//!
//! Parsing and storage stay in `juttusihti-core`; [`parse`] and [`apply`]
//! glue a fetch outcome onto the store.

pub mod tiles;

use std::time::Duration;

use juttusihti_core::discovery::{self, DiscoveredFeed};
use juttusihti_core::feed::{self, MAX_FEED_BYTES, ParsedFeed};
use juttusihti_core::store::Store;
use juttusihti_core::{FeedId, IngestStats};
use reqwest::header::{ETAG, HeaderMap, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, RETRY_AFTER};
use reqwest::{StatusCode, redirect};

pub const USER_AGENT: &str =
    concat!("Juttusihti/", env!("CARGO_PKG_VERSION"), " (+", env!("CARGO_PKG_REPOSITORY"), ")");

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("only http and https URLs can be fetched")]
    UnsupportedUrl,
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("server returned {0}")]
    Status(StatusCode),
    #[error("response is larger than {0} bytes")]
    TooLarge(usize),
    #[error("{0}")]
    Core(#[from] juttusihti_core::Error),
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub timeout: Duration,
    pub connect_timeout: Duration,
    /// Total attempts including the first one.
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub max_bytes: usize,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            max_attempts: 3,
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            max_bytes: MAX_FEED_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    NotModified,
    Fetched { body: Vec<u8>, final_url: String, etag: Option<String>, last_modified: Option<String> },
}

#[derive(Clone)]
pub struct Fetcher {
    client: reqwest::Client,
    policy: Policy,
}

impl Fetcher {
    pub fn new(policy: Policy) -> Result<Self, FetchError> {
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(policy.timeout)
            .connect_timeout(policy.connect_timeout)
            .redirect(redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    attempt.error("too many redirects")
                } else if !matches!(attempt.url().scheme(), "http" | "https") {
                    attempt.error("redirect to a non-web URL")
                } else {
                    attempt.follow()
                }
            }))
            .build()?;
        Ok(Self { client, policy })
    }

    /// Fetches a URL, sending cache validators when known. Retries
    /// connection failures, timeouts, 429 and 5xx responses.
    pub async fn fetch(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<FetchOutcome, FetchError> {
        if !feed::is_web_url(url) {
            return Err(FetchError::UnsupportedUrl);
        }
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.fetch_once(url, etag, last_modified).await {
                Ok(outcome) => return Ok(outcome),
                Err(Attempt::Retryable(e, retry_after)) if attempt < self.policy.max_attempts => {
                    let backoff = self.policy.initial_backoff * 2u32.pow(attempt - 1);
                    let wait = retry_after.unwrap_or(backoff).min(self.policy.max_backoff);
                    tracing::debug!(url, attempt, ?wait, error = %e, "retrying fetch");
                    tokio::time::sleep(wait).await;
                }
                Err(Attempt::Retryable(e, _) | Attempt::Fatal(e)) => return Err(e),
            }
        }
    }

    async fn fetch_once(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<FetchOutcome, Attempt> {
        let mut req = self.client.get(url).header(
            reqwest::header::ACCEPT,
            "application/rss+xml, application/atom+xml, application/feed+json, application/xml;q=0.9, text/xml;q=0.9, text/html;q=0.5, */*;q=0.1",
        );
        if let Some(e) = etag {
            req = req.header(IF_NONE_MATCH, e);
        }
        if let Some(lm) = last_modified {
            req = req.header(IF_MODIFIED_SINCE, lm);
        }
        let mut resp = req.send().await.map_err(classify)?;
        let status = resp.status();
        if status == StatusCode::NOT_MODIFIED {
            return Ok(FetchOutcome::NotModified);
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(Attempt::Retryable(FetchError::Status(status), retry_after(resp.headers())));
        }
        if !status.is_success() {
            return Err(Attempt::Fatal(FetchError::Status(status)));
        }
        let max = self.policy.max_bytes;
        if resp.content_length().is_some_and(|n| n as usize > max) {
            return Err(Attempt::Fatal(FetchError::TooLarge(max)));
        }
        let final_url = resp.url().to_string();
        let header = |name| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
        let (etag, last_modified) = (header(ETAG), header(LAST_MODIFIED));
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(classify)? {
            if body.len() + chunk.len() > max {
                return Err(Attempt::Fatal(FetchError::TooLarge(max)));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(FetchOutcome::Fetched { body, final_url, etag, last_modified })
    }

    /// Resolves a user-entered URL to feed URLs: the URL itself if it is a
    /// feed, otherwise the feeds its HTML page advertises.
    pub async fn discover(&self, url: &str) -> Result<Vec<DiscoveredFeed>, FetchError> {
        let url = normalize_input_url(url);
        match self.fetch(&url, None, None).await? {
            FetchOutcome::NotModified => Ok(Vec::new()),
            FetchOutcome::Fetched { body, final_url, .. } => {
                if discovery::looks_like_feed(&body) {
                    let title = feed::parse(&body, Some(&final_url)).ok().and_then(|f| f.title);
                    Ok(vec![DiscoveredFeed { url: final_url, title }])
                } else {
                    Ok(discovery::discover(&String::from_utf8_lossy(&body), &final_url))
                }
            }
        }
    }
}

enum Attempt {
    Retryable(FetchError, Option<Duration>),
    Fatal(FetchError),
}

fn classify(e: reqwest::Error) -> Attempt {
    if e.is_timeout() || e.is_connect() || e.is_request() && !e.is_builder() {
        Attempt::Retryable(e.into(), None)
    } else {
        Attempt::Fatal(e.into())
    }
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers.get(RETRY_AFTER)?.to_str().ok()?.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Adds `https://` to bare host names typed by the user.
pub fn normalize_input_url(input: &str) -> String {
    let t = input.trim();
    if t.contains("://") { t.to_string() } else { format!("https://{t}") }
}

/// A fetch outcome with its document parsed, ready for [`apply`].
#[derive(Debug)]
pub enum ParsedOutcome {
    NotModified,
    Feed { feed: ParsedFeed, etag: Option<String>, last_modified: Option<String> },
    Failed(FetchError),
}

/// Parses a fetched document on a blocking thread. Feeds are untrusted, so call this before
/// locking the store: a parser panic then fails this one feed and can't poison the store's lock.
pub async fn parse(outcome: Result<FetchOutcome, FetchError>) -> ParsedOutcome {
    parse_with(outcome, feed::parse).await
}

async fn parse_with(
    outcome: Result<FetchOutcome, FetchError>,
    parser: fn(&[u8], Option<&str>) -> juttusihti_core::Result<ParsedFeed>,
) -> ParsedOutcome {
    let (body, final_url, etag, last_modified) = match outcome {
        Ok(FetchOutcome::NotModified) => return ParsedOutcome::NotModified,
        Ok(FetchOutcome::Fetched { body, final_url, etag, last_modified }) => (body, final_url, etag, last_modified),
        Err(e) => return ParsedOutcome::Failed(e),
    };
    match tokio::task::spawn_blocking(move || parser(&body, Some(&final_url))).await {
        Ok(Ok(feed)) => ParsedOutcome::Feed { feed, etag, last_modified },
        Ok(Err(e)) => ParsedOutcome::Failed(e.into()),
        Err(e) => {
            tracing::error!(error = %e, "feed parser failed");
            let message = "internal error while reading this feed".to_string();
            ParsedOutcome::Failed(juttusihti_core::Error::FeedParse(message).into())
        }
    }
}

/// Stores a parsed fetch outcome for a feed: ingests entries and records
/// cache validators, or records the error.
pub fn apply(store: &mut Store, feed_id: FeedId, outcome: ParsedOutcome) -> Result<IngestStats, FetchError> {
    match outcome {
        ParsedOutcome::NotModified => {
            store.record_not_modified(feed_id)?;
            Ok(IngestStats::default())
        }
        ParsedOutcome::Feed { feed, etag, last_modified } => {
            let stats = store.ingest(feed_id, &feed.entries)?;
            store.adopt_feed_metadata(feed_id, feed.title.as_deref(), feed.site_url.as_deref())?;
            store.record_fetch_ok(feed_id, etag.as_deref(), last_modified.as_deref())?;
            Ok(stats)
        }
        ParsedOutcome::Failed(e) => {
            store.record_fetch_error(feed_id, &e.to_string())?;
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use juttusihti_core::NewFeed;

    use super::*;

    #[tokio::test]
    async fn a_parser_panic_fails_only_that_feed() {
        let mut store = Store::open_in_memory().unwrap();
        let url = "https://example.com/rss".to_string();
        let feed_id = store.add_feed(&NewFeed { url: url.clone(), ..Default::default() }).unwrap();
        let fetched = FetchOutcome::Fetched { body: vec![], final_url: url, etag: None, last_modified: None };

        let parsed = parse_with(Ok(fetched), |_, _| panic!("hostile feed")).await;

        assert!(apply(&mut store, feed_id, parsed).is_err());
        assert!(store.feed(feed_id).unwrap().last_error.unwrap().contains("internal error"));
    }
}

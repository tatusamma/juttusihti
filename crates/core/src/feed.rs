//! Parsing RSS 2.0, RSS 1.0, Atom and JSON Feed documents into storable entries.
//!
//! Feed content is untrusted: sizes are bounded and HTML is only ever
//! reduced to plain text for display and indexing.

use crate::{Error, NewArticle, Result};

/// Upper bound for a single feed document.
pub const MAX_FEED_BYTES: usize = 16 * 1024 * 1024;
/// Upper bound for one stored text field, to stop hostile feeds bloating the database.
pub const MAX_FIELD_CHARS: usize = 512 * 1024;
/// Upper bound for entries taken from one document.
pub const MAX_ENTRIES: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFeed {
    pub title: Option<String>,
    pub site_url: Option<String>,
    pub entries: Vec<NewArticle>,
}

/// Parses a feed document. `source_url` resolves relative links.
pub fn parse(bytes: &[u8], source_url: Option<&str>) -> Result<ParsedFeed> {
    if bytes.len() > MAX_FEED_BYTES {
        return Err(Error::FeedParse(format!("feed is larger than {MAX_FEED_BYTES} bytes")));
    }
    let parser = feed_rs::parser::Builder::new().base_uri(source_url).sanitize_content(true).build();
    let feed = parser.parse(bytes).map_err(|e| Error::FeedParse(e.to_string()))?;

    let site_url = feed
        .links
        .iter()
        .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
        .map(|l| l.href.clone())
        .filter(|h| is_web_url(h));

    let entries = feed
        .entries
        .into_iter()
        .take(MAX_ENTRIES)
        .filter_map(|e| {
            let url = e
                .links
                .iter()
                .find(|l| l.rel.as_deref().is_none_or(|r| r == "alternate"))
                .or_else(|| e.links.first())
                .map(|l| l.href.trim().to_string())
                .filter(|h| is_web_url(h));
            let summary = e.summary.map(|t| bounded(t.content));
            let content = e.content.and_then(|c| c.body).map(bounded);
            let title = e
                .title
                .map(|t| crate::normalize::html_to_text(&t.content))
                .filter(|t| !t.is_empty())
                .or_else(|| summary.as_deref().map(first_words))
                .unwrap_or_default();
            if title.is_empty() && url.is_none() {
                return None;
            }
            let author = e.authors.into_iter().filter_map(|p| p.name).next();
            let published_at = e.published.or(e.updated).map(|d| d.timestamp());
            let guid = if e.id.trim().is_empty() {
                url.clone().unwrap_or_else(|| crate::normalize::sha256_hex(&title))
            } else {
                e.id
            };
            Some(NewArticle { guid, url, title: bounded(title), summary, content, author, published_at })
        })
        .collect();

    Ok(ParsedFeed {
        title: feed.title.map(|t| crate::normalize::html_to_text(&t.content)).filter(|t| !t.is_empty()),
        site_url,
        entries,
    })
}

/// Only http(s) links are kept; `javascript:`, `file:` and others are dropped.
pub fn is_web_url(s: &str) -> bool {
    url::Url::parse(s).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
}

/// Truncates text to [`MAX_FIELD_CHARS`] bytes at a character boundary.
pub(crate) fn bounded(mut s: String) -> String {
    if s.len() > MAX_FIELD_CHARS {
        let mut cut = MAX_FIELD_CHARS;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

fn first_words(html: &str) -> String {
    let text = crate::normalize::html_to_text(html);
    let mut out: String = text.split_whitespace().take(12).collect::<Vec<_>>().join(" ");
    if text.split_whitespace().count() > 12 {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = include_str!("../tests/fixtures/yle_fi.rss");
    const ATOM: &str = include_str!("../tests/fixtures/example.atom");
    const JSON: &str = include_str!("../tests/fixtures/example.json");

    #[test]
    fn parses_finnish_rss() {
        let f = parse(RSS.as_bytes(), Some("https://example.fi/rss")).unwrap();
        assert_eq!(f.title.as_deref(), Some("Uutiset – Esimerkki"));
        assert_eq!(f.entries.len(), 3);
        let first = &f.entries[0];
        assert_eq!(first.title, "Hallitus esittää muutoksia työttömyysturvaan");
        assert_eq!(
            first.url.as_deref(),
            Some("https://example.fi/uutiset/1?utm_source=rss"),
            "stored URL stays original"
        );
        assert!(first.published_at.is_some());
        // Entry with no title falls back to the description.
        assert!(f.entries[2].title.starts_with("Pakkanen kiristyy"));
    }

    #[test]
    fn parses_atom_and_drops_unsafe_links() {
        let f = parse(ATOM.as_bytes(), None).unwrap();
        assert_eq!(f.entries.len(), 2);
        assert_eq!(f.entries[0].author.as_deref(), Some("Jane Reporter"));
        assert!(f.entries[0].content.as_deref().unwrap().contains("Council"));
        assert_eq!(f.entries[1].url, None, "javascript: link must be dropped");
    }

    #[test]
    fn parses_json_feed() {
        let f = parse(JSON.as_bytes(), None).unwrap();
        assert_eq!(f.entries.len(), 1);
        assert_eq!(f.entries[0].guid, "json-1");
    }

    #[test]
    fn rejects_garbage_and_oversized() {
        assert!(parse(b"<html>not a feed</html>", None).is_err());
        let big = vec![b' '; MAX_FEED_BYTES + 1];
        assert!(parse(&big, None).is_err());
    }
}

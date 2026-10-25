//! Feed discovery: finding feed URLs advertised by an HTML page through
//! `<link rel="alternate" type="application/rss+xml" href="...">`.

use url::Url;

const FEED_TYPES: &[&str] =
    &["application/rss+xml", "application/atom+xml", "application/feed+json", "application/json"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredFeed {
    pub url: String,
    pub title: Option<String>,
}

/// Returns true if the bytes look like a feed rather than an HTML page.
pub fn looks_like_feed(body: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&body[..body.len().min(1024)]).to_ascii_lowercase();
    head.contains("<rss") || head.contains("<feed") || head.contains("<rdf:rdf") || head.contains("jsonfeed.org")
}

/// Finds advertised feeds in an HTML document, resolving relative URLs
/// against `page_url`. Only http(s) results are returned.
pub fn discover(html: &str, page_url: &str) -> Vec<DiscoveredFeed> {
    let Ok(base) = Url::parse(page_url) else { return Vec::new() };
    let lower = html.to_ascii_lowercase();
    let mut found = Vec::new();
    let mut pos = 0;
    while let Some(start) = lower[pos..].find("<link") {
        let start = pos + start;
        let Some(end) = lower[start..].find('>') else { break };
        let end = start + end;
        let tag = &html[start..end];
        pos = end;
        let attr = |name: &str| attribute(tag, name);
        let rel_ok = attr("rel").is_some_and(|r| r.to_ascii_lowercase().split_whitespace().any(|t| t == "alternate"));
        let type_ok = attr("type").is_some_and(|t| FEED_TYPES.contains(&t.to_ascii_lowercase().trim()));
        if !(rel_ok && type_ok) {
            continue;
        }
        let Some(href) = attr("href") else { continue };
        let Ok(url) = base.join(href.trim()) else { continue };
        if !matches!(url.scheme(), "http" | "https") {
            continue;
        }
        let url = url.to_string();
        if !found.iter().any(|f: &DiscoveredFeed| f.url == url) {
            found.push(DiscoveredFeed { url, title: attr("title").map(crate::normalize::html_to_text) });
        }
    }
    found
}

/// Extracts an attribute value from a single tag's source text.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let lower = tag.to_ascii_lowercase();
    let mut search = 0;
    while let Some(i) = lower[search..].find(name) {
        let i = search + i;
        search = i + name.len();
        let before_ok = i == 0 || lower.as_bytes()[i - 1].is_ascii_whitespace();
        let rest = lower[i + name.len()..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let value_start = tag.len() - rest.len() + 1;
        let value = tag[value_start..].trim_start();
        let offset = tag.len() - value.len();
        return match value.chars().next()? {
            q @ ('"' | '\'') => {
                let close = value[1..].find(q)?;
                Some(&tag[offset + 1..offset + 1 + close])
            }
            _ => {
                let len = value.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(value.len());
                Some(&tag[offset..offset + len])
            }
        };
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_rss_and_atom_links() {
        let html = r#"<html><head>
            <link rel="stylesheet" href="/s.css">
            <LINK REL="alternate" TYPE="application/rss+xml" title="Uutiset &amp; sää" href="/rss.xml">
            <link type='application/atom+xml' rel='alternate home' href='https://other.example/atom'/>
            <link rel="alternate" type="application/rss+xml" href="javascript:alert(1)">
            <link rel=alternate type=application/rss+xml href=/rss.xml>
        </head></html>"#;
        let feeds = discover(html, "https://news.example/section/page.html");
        assert_eq!(
            feeds,
            vec![
                DiscoveredFeed { url: "https://news.example/rss.xml".into(), title: Some("Uutiset & sää".into()) },
                DiscoveredFeed { url: "https://other.example/atom".into(), title: None },
            ]
        );
    }

    #[test]
    fn detects_feed_bodies() {
        assert!(looks_like_feed(b"<?xml version=\"1.0\"?><rss version=\"2.0\">"));
        assert!(looks_like_feed(b"<feed xmlns=\"http://www.w3.org/2005/Atom\">"));
        assert!(!looks_like_feed(b"<!doctype html><html>"));
    }
}

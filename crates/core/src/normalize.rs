//! Deterministic normalization used for deduplication and vocabulary control.

use sha2::{Digest, Sha256};
use url::Url;

/// Query parameters that only track campaigns and never identify content.
const TRACKING_PARAMS: &[&str] = &[
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "utm_id",
    "fbclid",
    "gclid",
    "mc_cid",
    "mc_eid",
    "ref",
    "ref_src",
    "cmpid",
];

/// Canonical form of an article URL: lowercase scheme/host, no fragment,
/// no tracking parameters, remaining parameters sorted, no trailing slash.
pub fn canonical_url(raw: &str) -> Option<String> {
    let mut url = Url::parse(raw.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_fragment(None);
    if url.path().len() > 1 && url.path().ends_with('/') {
        let trimmed = url.path().trim_end_matches('/').to_string();
        url.set_path(&trimmed);
    }
    let mut params: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !TRACKING_PARAMS.contains(&k.to_ascii_lowercase().as_str()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    params.sort();
    if params.is_empty() {
        url.set_query(None);
    } else {
        url.query_pairs_mut().clear().extend_pairs(params);
    }
    let _ = url.set_scheme("https");
    Some(url.to_string())
}

/// Lowercased text with collapsed whitespace and punctuation removed.
/// Keeps all Unicode letters and digits (ä, ö, å, etc.).
pub fn normalize_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for ch in s.chars() {
        if ch.is_alphanumeric() {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.extend(ch.to_lowercase());
        } else {
            pending_space = true;
        }
    }
    out
}

/// Dedup key: the canonical URL if there is one, otherwise a hash of the
/// normalized title. Syndicated copies of one story usually share it.
pub fn dedup_key(url: Option<&str>, title: &str) -> String {
    match url.and_then(canonical_url) {
        Some(u) => format!("url:{u}"),
        None => format!("title:{}", sha256_hex(&normalize_text(title))),
    }
}

/// Hash of the source content fields, used to detect real updates and as
/// part of AI cache keys.
pub fn content_hash(title: &str, summary: Option<&str>, content: Option<&str>) -> String {
    let mut h = Sha256::new();
    for part in [Some(title), summary, content] {
        h.update(part.unwrap_or("").as_bytes());
        h.update([0u8]);
    }
    hex(&h.finalize())
}

pub fn sha256_hex(s: &str) -> String {
    hex(&Sha256::digest(s.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Strips HTML tags and decodes the common entities, for previews and
/// search snippets. This is not a sanitizer for rendering HTML.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut skip_until: Option<&str> = None;
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if let Some(end) = skip_until {
            match lower[i..].find(end) {
                Some(pos) => {
                    i += pos + end.len();
                    skip_until = None;
                }
                None => break,
            }
            continue;
        }
        let c = bytes[i];
        if in_tag {
            if c == b'>' {
                in_tag = false;
            }
            i += 1;
            continue;
        }
        if c == b'<' {
            let rest = &lower[i..];
            if rest.starts_with("<script") {
                skip_until = Some("</script>");
            } else if rest.starts_with("<style") {
                skip_until = Some("</style>");
            } else {
                if rest.starts_with("<br")
                    || rest.starts_with("<p")
                    || rest.starts_with("</p")
                    || rest.starts_with("<li")
                    || rest.starts_with("<div")
                    || rest.starts_with("</div")
                {
                    out.push('\n');
                }
                in_tag = true;
            }
            i += 1;
            continue;
        }
        // Copy one UTF-8 character.
        let ch_len = html[i..].chars().next().map_or(1, char::len_utf8);
        out.push_str(&html[i..i + ch_len]);
        i += ch_len;
    }
    let decoded = decode_entities(&out);
    // Collapse runs of blank lines and trim spaces on each line.
    let mut result = String::with_capacity(decoded.len());
    let mut blank = 0;
    for line in decoded.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            blank += 1;
            if blank == 1 && !result.is_empty() {
                result.push('\n');
            }
        } else {
            blank = 0;
            if !result.is_empty() && !result.ends_with('\n') {
                result.push('\n');
            }
            result.push_str(&line);
        }
    }
    result.trim().to_string()
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..semi];
        let decoded = match name {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            "auml" => Some('ä'),
            "ouml" => Some('ö'),
            "aring" => Some('å'),
            "Auml" => Some('Ä'),
            "Ouml" => Some('Ö'),
            "Aring" => Some('Å'),
            "hellip" => Some('…'),
            "ndash" => Some('–'),
            "mdash" => Some('—'),
            _ if name.starts_with("#x") || name.starts_with("#X") => {
                u32::from_str_radix(&name[2..], 16).ok().and_then(char::from_u32)
            }
            _ if name.starts_with('#') => name[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_url_strips_tracking_and_fragment() {
        assert_eq!(
            canonical_url("HTTP://Example.COM/a/b/?utm_source=x&b=2&a=1#top").as_deref(),
            Some("https://example.com/a/b?a=1&b=2")
        );
        assert_eq!(canonical_url("https://yle.fi/").as_deref(), Some("https://yle.fi/"));
        assert_eq!(canonical_url("javascript:alert(1)"), None);
        assert_eq!(canonical_url("not a url"), None);
    }

    #[test]
    fn normalize_keeps_finnish_letters() {
        assert_eq!(normalize_text("  Sää  huononee —  Pohjois-Suomessa! "), "sää huononee pohjois suomessa");
        assert_eq!(normalize_text("ÅLAND"), "åland");
    }

    #[test]
    fn dedup_key_prefers_url() {
        let a = dedup_key(Some("https://ex.com/x?utm_source=rss"), "A");
        let b = dedup_key(Some("https://ex.com/x"), "Different title");
        assert_eq!(a, b);
        assert_eq!(dedup_key(None, "Hello, World"), dedup_key(None, "hello world"));
    }

    #[test]
    fn content_hash_distinguishes_fields() {
        assert_ne!(content_hash("ab", Some("c"), None), content_hash("a", Some("bc"), None));
        assert_eq!(content_hash("a", None, None), content_hash("a", Some(""), None));
    }

    #[test]
    fn html_to_text_strips_tags_scripts_and_entities() {
        let html = "<p>Hei &amp; tervetuloa</p><script>alert('x')</script><p>S&auml;&auml; &#8211; ok&nbsp;!</p>";
        assert_eq!(html_to_text(html), "Hei & tervetuloa\nSää – ok !");
        assert_eq!(html_to_text("a &bogus; b & c"), "a &bogus; b & c");
    }
}

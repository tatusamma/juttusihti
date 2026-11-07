//! Evidence sources for the fact check: Google's Fact Check Tools API (needs
//! the user's own key) and Wikipedia search (keyless). Both only receive a
//! short search phrase, never the article.

use std::time::Duration;

use serde_json::Value;

const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_EXTRACT_CHARS: usize = 500;

/// A fact check someone else published, with the rating exactly as they wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedCheck {
    pub claim_text: String,
    pub claimant: Option<String>,
    pub publisher: String,
    pub url: String,
    pub title: String,
    pub rating: String,
    pub review_date: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiPage {
    pub title: String,
    pub url: String,
    pub extract: String,
}

/// Returns `lang` when it looks like a language code (`^[a-z]{2,3}$`), else `"en"`.
pub fn valid_lang(lang: &str) -> &str {
    if (2..=3).contains(&lang.len()) && lang.bytes().all(|b| b.is_ascii_lowercase()) { lang } else { "en" }
}

pub struct Sources {
    http: reqwest::Client,
    google_key: Option<String>,
    google_base: String,
    wikipedia_base: String,
}

impl Sources {
    pub fn new(google_key: Option<String>) -> Result<Self, String> {
        Self::with_bases(google_key, "https://factchecktools.googleapis.com", "https://{lang}.wikipedia.org")
    }

    /// `wikipedia_base` contains `{lang}`; the bases exist so tests can point at a local server.
    pub fn with_bases(google_key: Option<String>, google_base: &str, wikipedia_base: &str) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("Juttusihti/", env!("CARGO_PKG_VERSION"), " (https://github.com/tero-k/juttusihti)"))
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.without_url().to_string())?;
        Ok(Self {
            http,
            google_key: google_key.filter(|k| !k.trim().is_empty()),
            google_base: google_base.trim_end_matches('/').to_string(),
            wikipedia_base: wikipedia_base.trim_end_matches('/').to_string(),
        })
    }

    pub fn has_google(&self) -> bool {
        self.google_key.is_some()
    }

    /// Published fact checks for `query`. Tries `lang` first, then any language if nothing came back.
    pub async fn fact_checks(&self, query: &str, lang: &str) -> Result<Vec<PublishedCheck>, String> {
        let Some(key) = &self.google_key else { return Ok(Vec::new()) };
        let lang = valid_lang(lang);
        let found = self.google_search(key, query, Some(lang)).await?;
        if !found.is_empty() {
            return Ok(found);
        }
        self.google_search(key, query, None).await
    }

    async fn google_search(&self, key: &str, query: &str, lang: Option<&str>) -> Result<Vec<PublishedCheck>, String> {
        let mut url = url::Url::parse(&format!("{}/v1alpha1/claims:search", self.google_base))
            .map_err(|_| "invalid Google Fact Check address".to_string())?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("query", query);
            if let Some(lang) = lang {
                q.append_pair("languageCode", lang);
            }
            q.append_pair("pageSize", "5");
            q.append_pair("key", key);
        }
        let body = self.get(url).await?;
        parse_claims(&body)
    }

    /// The top Wikipedia pages for `query` in the article's language.
    pub async fn wikipedia(&self, query: &str, lang: &str) -> Result<Vec<WikiPage>, String> {
        let base = self.wikipedia_base.replace("{lang}", valid_lang(lang));
        let mut url = url::Url::parse(&format!("{base}/w/rest.php/v1/search/page"))
            .map_err(|_| "invalid Wikipedia address".to_string())?;
        url.query_pairs_mut().append_pair("q", query).append_pair("limit", "2");
        let body = self.get(url).await?;
        parse_wikipedia(&body, &base)
    }

    /// GET with a body cap. Error text never carries the URL (it holds the Google key).
    async fn get(&self, url: url::Url) -> Result<Vec<u8>, String> {
        let mut resp = self.http.get(url).send().await.map_err(|e| e.without_url().to_string())?;
        let status = resp.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| e.without_url().to_string())? {
            if bytes.len() + chunk.len() > MAX_BODY_BYTES {
                return Err("response too large".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(format!("request failed with HTTP {}", status.as_u16()));
        }
        Ok(bytes)
    }
}

fn text(v: &Value, field: &str) -> Option<String> {
    v.get(field).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn parse_claims(body: &[u8]) -> Result<Vec<PublishedCheck>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "unreadable Google Fact Check reply".to_string())?;
    let mut out = Vec::new();
    for claim in v.get("claims").and_then(Value::as_array).into_iter().flatten() {
        let claim_text = text(claim, "text").unwrap_or_default();
        let claimant = text(claim, "claimant");
        for review in claim.get("claimReview").and_then(Value::as_array).into_iter().flatten() {
            let Some(url) = text(review, "url") else { continue };
            let publisher =
                review.get("publisher").and_then(|p| text(p, "name").or_else(|| text(p, "site"))).unwrap_or_default();
            out.push(PublishedCheck {
                claim_text: claim_text.clone(),
                claimant: claimant.clone(),
                publisher,
                url,
                title: text(review, "title").unwrap_or_default(),
                rating: text(review, "textualRating").unwrap_or_default(),
                review_date: text(review, "reviewDate"),
            });
        }
    }
    Ok(out)
}

fn parse_wikipedia(body: &[u8], base: &str) -> Result<Vec<WikiPage>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "unreadable Wikipedia reply".to_string())?;
    let mut out = Vec::new();
    for page in v.get("pages").and_then(Value::as_array).into_iter().flatten() {
        let (Some(key), Some(title)) = (text(page, "key"), text(page, "title")) else { continue };
        let Ok(mut url) = url::Url::parse(&format!("{base}/wiki/")) else { continue };
        if let Ok(mut segs) = url.path_segments_mut() {
            segs.pop_if_empty().push(&key);
        }
        let extract = strip_tags(page.get("excerpt").and_then(Value::as_str).unwrap_or(""));
        out.push(WikiPage { title, url: url.to_string(), extract });
    }
    Ok(out)
}

/// Removes HTML tags, decodes the common entities and bounds the result.
fn strip_tags(html: &str) -> String {
    let mut plain = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => plain.push(c),
            _ => {}
        }
    }
    let plain = plain
        .replace("&quot;", "\"")
        .replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    let plain = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    plain.chars().take(MAX_EXTRACT_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claim_search_reply() {
        let body = r#"{"claims":[{"text":"Kuu on juustoa","claimant":"Joku","claimDate":"2024-01-01T00:00:00Z",
            "claimReview":[
              {"publisher":{"name":"Faktabaari","site":"faktabaari.fi"},"url":"https://faktabaari.fi/a","title":"Kuu","reviewDate":"2024-02-01T00:00:00Z","textualRating":"Väärä","languageCode":"fi"},
              {"publisher":{"site":"other.example"},"url":"https://other.example/b","title":"Moon","textualRating":"False"}]}],
            "nextPageToken":"x"}"#;
        let checks = parse_claims(body.as_bytes()).unwrap();
        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].publisher, "Faktabaari");
        assert_eq!(checks[0].rating, "Väärä");
        assert_eq!(checks[0].claim_text, "Kuu on juustoa");
        assert_eq!(checks[0].claimant.as_deref(), Some("Joku"));
        assert_eq!(checks[0].review_date.as_deref(), Some("2024-02-01T00:00:00Z"));
        assert_eq!(checks[1].publisher, "other.example");
        assert_eq!(checks[1].review_date, None);
        assert!(parse_claims(b"{}").unwrap().is_empty());
    }

    #[test]
    fn parses_wikipedia_search_and_strips_tags() {
        let body = r#"{"pages":[{"id":1,"key":"Kuu (taivaankappale)","title":"Kuu","excerpt":"The <span class=\"searchmatch\">Moon</span> is &quot;Earth&quot; satellite","matched_title":null,"description":"d","thumbnail":null}]}"#;
        let pages = parse_wikipedia(body.as_bytes(), "https://fi.wikipedia.org").unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].title, "Kuu");
        assert_eq!(pages[0].extract, "The Moon is \"Earth\" satellite");
        assert_eq!(pages[0].url, "https://fi.wikipedia.org/wiki/Kuu%20(taivaankappale)");
        let long = format!(r#"{{"pages":[{{"key":"A","title":"A","excerpt":"{}"}}]}}"#, "ä".repeat(900));
        assert_eq!(parse_wikipedia(long.as_bytes(), "https://x").unwrap()[0].extract.chars().count(), 500);
    }

    #[test]
    fn invalid_lang_falls_back_to_english() {
        assert_eq!(valid_lang("fi"), "fi");
        assert_eq!(valid_lang("fil"), "fil");
        for bad in ["", "e", "english", "EN", "e1", "../x", "fi.evil.com"] {
            assert_eq!(valid_lang(bad), "en", "{bad}");
        }
    }
}

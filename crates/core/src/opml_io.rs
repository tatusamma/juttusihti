//! OPML subscription import and export.

use opml::{Body, Head, OPML, Outline};

use crate::store::Store;
use crate::{Error, NewFeed, Result};

/// Upper bound for an imported OPML document.
pub const MAX_OPML_BYTES: usize = 8 * 1024 * 1024;
const MAX_IMPORTED_FEEDS: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpmlFeed {
    /// Folder path; nested outlines are joined with " / ".
    pub folder: Option<String>,
    pub title: String,
    pub url: String,
    pub site_url: Option<String>,
}

/// Extracts http(s) feed subscriptions from an OPML document.
pub fn parse(xml: &str) -> Result<Vec<OpmlFeed>> {
    if xml.len() > MAX_OPML_BYTES {
        return Err(Error::Opml("OPML file is too large".into()));
    }
    let doc = OPML::from_str(xml).map_err(|e| Error::Opml(e.to_string()))?;
    let mut out = Vec::new();
    walk(&doc.body.outlines, None, &mut out);
    out.truncate(MAX_IMPORTED_FEEDS);
    Ok(out)
}

fn walk(outlines: &[Outline], folder: Option<&str>, out: &mut Vec<OpmlFeed>) {
    for o in outlines {
        let label = o.title.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(|| o.text.clone());
        match o.xml_url.as_deref().map(str::trim) {
            Some(url) if crate::feed::is_web_url(url) => out.push(OpmlFeed {
                folder: folder.map(str::to_string),
                title: label.trim().to_string(),
                url: url.to_string(),
                site_url: o.html_url.clone().filter(|u| crate::feed::is_web_url(u)),
            }),
            Some(_) => {}
            None if !o.outlines.is_empty() => {
                let name = label.trim();
                let path = match (folder, name.is_empty()) {
                    (Some(parent), false) => format!("{parent} / {name}"),
                    (Some(parent), true) => parent.to_string(),
                    (None, false) => name.to_string(),
                    (None, true) => String::new(),
                };
                walk(&o.outlines, (!path.is_empty()).then_some(path.as_str()), out);
            }
            None => {}
        }
    }
}

/// Imports subscriptions, creating folders as needed. Returns
/// (feeds in file, newly added feeds).
pub fn import(store: &Store, xml: &str) -> Result<(usize, usize)> {
    let feeds = parse(xml)?;
    let before = store.feeds()?.len();
    for f in &feeds {
        let folder_id = f.folder.as_deref().map(|name| store.create_folder(name)).transpose()?;
        store.add_feed(&NewFeed {
            url: f.url.clone(),
            title: f.title.clone(),
            site_url: f.site_url.clone(),
            folder_id,
        })?;
    }
    Ok((feeds.len(), store.feeds()?.len() - before))
}

/// Exports all subscriptions, grouped by folder.
pub fn export(store: &Store) -> Result<String> {
    let folders = store.folders()?;
    let feeds = store.feeds()?;
    let outline = |f: &crate::Feed| Outline {
        text: f.title.clone(),
        title: Some(f.title.clone()),
        r#type: Some("rss".into()),
        xml_url: Some(f.url.clone()),
        html_url: f.site_url.clone(),
        ..Default::default()
    };
    let mut outlines: Vec<Outline> = folders
        .iter()
        .map(|folder| Outline {
            text: folder.name.clone(),
            title: Some(folder.name.clone()),
            outlines: feeds.iter().filter(|f| f.folder_id == Some(folder.id)).map(outline).collect(),
            ..Default::default()
        })
        .filter(|o| !o.outlines.is_empty())
        .collect();
    outlines.extend(feeds.iter().filter(|f| f.folder_id.is_none()).map(outline));
    let doc = OPML {
        version: "2.0".into(),
        head: Some(Head { title: Some("Juttusihti subscriptions".into()), ..Default::default() }),
        body: Body { outlines },
    };
    doc.to_string().map_err(|e| Error::Opml(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0"?>
<opml version="2.0"><head><title>t</title></head><body>
  <outline text="Uutiset">
    <outline text="Yle" title="Yle Uutiset" type="rss" xmlUrl="https://yle.example/rss" htmlUrl="https://yle.example/"/>
    <outline text="Talous"><outline text="Kauppa" xmlUrl="https://kauppa.example/feed"/></outline>
  </outline>
  <outline text="Loose" xmlUrl="https://loose.example/atom"/>
  <outline text="Evil" xmlUrl="file:///etc/passwd"/>
</body></opml>"#;

    #[test]
    fn parses_nested_folders_and_skips_unsafe_urls() {
        let feeds = parse(SAMPLE).unwrap();
        assert_eq!(feeds.len(), 3);
        assert_eq!(feeds[0].title, "Yle Uutiset");
        assert_eq!(feeds[0].folder.as_deref(), Some("Uutiset"));
        assert_eq!(feeds[1].folder.as_deref(), Some("Uutiset / Talous"));
        assert_eq!(feeds[2].folder, None);
    }

    #[test]
    fn import_export_round_trip() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(import(&store, SAMPLE).unwrap(), (3, 3));
        // Re-import is idempotent.
        assert_eq!(import(&store, SAMPLE).unwrap(), (3, 0));
        let xml = export(&store).unwrap();
        let again = parse(&xml).unwrap();
        assert_eq!(again.len(), 3);
        let other = Store::open_in_memory().unwrap();
        assert_eq!(import(&other, &xml).unwrap(), (3, 3));
        assert_eq!(other.folders().unwrap().len(), 2);
    }

    #[test]
    fn rejects_malformed() {
        assert!(parse("<opml><body>").is_err());
    }
}

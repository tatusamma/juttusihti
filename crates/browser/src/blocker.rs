//! Tracker and ad blocking with the bundled EasyList and EasyPrivacy.
//!
//! The lists are pinned copies shipped with the app (see `assets/LICENSES.md`); nothing is ever
//! downloaded at run time.

use adblock::Engine;
use adblock::lists::{FilterSet, ParseOptions, RuleTypes};
use adblock::request::Request;

const EASYLIST: &str = include_str!("../assets/easylist.txt");
const EASYPRIVACY: &str = include_str!("../assets/easyprivacy.txt");

/// What a resource is loaded as, in the terms the filter lists use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    Document,
    Subdocument,
    Script,
    Stylesheet,
    Image,
    Font,
    Media,
    Object,
    /// `fetch()`, `XMLHttpRequest`, beacons and other loads without a destination.
    Xhr,
    Other,
}

impl ResourceKind {
    /// The request type name adblock-rust expects.
    fn filter_type(self) -> &'static str {
        match self {
            ResourceKind::Document => "document",
            ResourceKind::Subdocument => "subdocument",
            ResourceKind::Script => "script",
            ResourceKind::Stylesheet => "stylesheet",
            ResourceKind::Image => "image",
            ResourceKind::Font => "font",
            ResourceKind::Media => "media",
            ResourceKind::Object => "object",
            ResourceKind::Xhr => "xmlhttprequest",
            ResourceKind::Other => "other",
        }
    }
}

pub struct Blocker {
    engine: Engine,
}

impl Blocker {
    /// A blocker with the network rules of the bundled EasyList and EasyPrivacy.
    pub fn bundled() -> Blocker {
        let opts = ParseOptions { rule_types: RuleTypes::NetworkOnly, ..ParseOptions::default() };
        let mut filters = FilterSet::new(false);
        filters.add_filter_list(EASYLIST.to_string(), opts);
        filters.add_filter_list(EASYPRIVACY.to_string(), opts);
        Blocker { engine: Engine::new_with_filter_set(filters) }
    }

    /// Whether a load of `url` as `kind`, made by the page at `source_url`, matches a block rule
    /// (and no exception rule). URLs the matcher cannot parse are not blocked.
    pub fn should_block(&self, url: &str, source_url: &str, kind: ResourceKind) -> bool {
        match Request::new(url, source_url, kind.filter_type(), "GET") {
            Ok(request) => self.engine.check_network_request(&request).should_block(),
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_known_trackers_allows_article() {
        let blocker = Blocker::bundled();
        assert!(blocker.should_block(
            "https://www.google-analytics.com/analytics.js",
            "https://www.is.fi/x",
            ResourceKind::Script
        ));
        assert!(!blocker.should_block("https://www.is.fi/x.html", "https://www.is.fi/x", ResourceKind::Document));
        assert!(!blocker.should_block(
            "https://images.sanoma-sndp.fi/a.jpg",
            "https://www.is.fi/x",
            ResourceKind::Image
        ));
    }
}

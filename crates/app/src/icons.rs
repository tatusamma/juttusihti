//! Bundled Lucide icon font and the icons the interface uses.

use iced::Font;
use iced::widget::{Text, text};

use crate::style::SIZE_BODY;

/// The Lucide icon font (ISC licence, see `assets/LUCIDE-LICENSE.txt`).
pub const FONT_BYTES: &[u8] = include_bytes!("../assets/lucide.ttf");

const FONT: Font = Font::with_name("lucide");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    News,
    Saved,
    Reports,
    Review,
    Timeline,
    Map,
    Connections,
    Analysis,
    AiSetup,
    Labels,
    Settings,
    Refresh,
    More,
    Sparkles,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    Close,
    Star,
    Edit,
    External,
    Help,
    Check,
}

/// Codepoints from lucide-static 1.55.0 `font/lucide.css`.
fn codepoint(i: Icon) -> char {
    match i {
        Icon::News => '\u{e348}',         // newspaper
        Icon::Saved => '\u{e060}',        // bookmark
        Icon::Reports => '\u{e0cc}',      // file-text
        Icon::Review => '\u{e219}',       // clipboard-check
        Icon::Timeline => '\u{e1f5}',     // history
        Icon::Map => '\u{e110}',          // map
        Icon::Connections => '\u{e156}',  // share-2
        Icon::Analysis => '\u{e2a3}',     // chart-column
        Icon::AiSetup => '\u{e1bb}',      // bot
        Icon::Labels => '\u{e17f}',       // tag
        Icon::Settings => '\u{e154}',     // settings
        Icon::Refresh => '\u{e145}',      // refresh-cw
        Icon::More => '\u{e0b6}',         // ellipsis
        Icon::Sparkles => '\u{e412}',     // sparkles
        Icon::ChevronDown => '\u{e06d}',  // chevron-down
        Icon::ChevronLeft => '\u{e06e}',  // chevron-left
        Icon::ChevronRight => '\u{e06f}', // chevron-right
        Icon::Close => '\u{e1b2}',        // x
        Icon::Star => '\u{e176}',         // star
        Icon::Edit => '\u{e1f9}',         // pencil
        Icon::External => '\u{e0b9}',     // external-link
        Icon::Help => '\u{e082}',         // circle-question-mark
        Icon::Check => '\u{e06c}',        // check
    }
}

pub fn icon<'a>(i: Icon) -> Text<'a> {
    text(codepoint(i).to_string()).font(FONT).size(SIZE_BODY + 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Icon; 23] = [
        Icon::News,
        Icon::Saved,
        Icon::Reports,
        Icon::Review,
        Icon::Timeline,
        Icon::Map,
        Icon::Connections,
        Icon::Analysis,
        Icon::AiSetup,
        Icon::Labels,
        Icon::Settings,
        Icon::Refresh,
        Icon::More,
        Icon::Sparkles,
        Icon::ChevronDown,
        Icon::ChevronLeft,
        Icon::ChevronRight,
        Icon::Close,
        Icon::Star,
        Icon::Edit,
        Icon::External,
        Icon::Help,
        Icon::Check,
    ];

    #[test]
    fn every_icon_has_a_distinct_glyph() {
        let mut seen = std::collections::HashSet::new();
        for i in ALL {
            let c = codepoint(i);
            assert!(('\u{e000}'..='\u{f8ff}').contains(&c), "{i:?} is outside the private use area");
            assert!(seen.insert(c), "{i:?} shares its glyph with another icon");
        }
    }
}

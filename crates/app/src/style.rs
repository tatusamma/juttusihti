//! Calm "warm paper & amber" design tokens and the custom Iced themes built from them.
// Consumed by later restyle tasks; remove once every token is used.
#![allow(dead_code)]

use iced::theme::Palette;
use iced::{Color, Theme};

const DARK_NAME: &str = "Juttusihti Dark";
const LIGHT_NAME: &str = "Juttusihti Light";

/// Colour tokens for one theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    pub background: Color,
    pub pane: Color,
    pub line: Color,
    pub text: Color,
    pub body: Color,
    pub muted: Color,
    pub faint: Color,
    pub selection: Color,
    pub accent: Color,
    pub success: Color,
    pub danger: Color,
}

const fn hex(rgb: u32) -> Color {
    Color::from_rgb8((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const DARK: Tokens = Tokens {
    background: hex(0x1c1b1a),
    pane: hex(0x222120),
    line: hex(0x2f2d2b),
    text: hex(0xebe6df),
    body: hex(0xd6d1c9),
    muted: hex(0x9b958c),
    faint: hex(0x7a746c),
    selection: hex(0x2b2926),
    accent: hex(0xd8a25a),
    success: hex(0x8fa36b),
    danger: hex(0xd27a66),
};

pub const LIGHT: Tokens = Tokens {
    background: hex(0xfaf7f2),
    pane: hex(0xf1ece4),
    line: hex(0xe4ddd2),
    text: hex(0x2a2622),
    body: hex(0x3d3832),
    muted: hex(0x6e665b),
    faint: hex(0x877f75),
    selection: hex(0xefe6d8),
    accent: hex(0xb0702a),
    success: hex(0x5f7a3e),
    danger: hex(0xb04a38),
};

/// The tokens matching an Iced theme: light for "Juttusihti Light", dark otherwise.
pub fn tokens(theme: &Theme) -> &'static Tokens {
    if theme.to_string() == LIGHT_NAME { &LIGHT } else { &DARK }
}

/// The custom Iced theme for the dark or light token set.
pub fn theme(dark: bool) -> Theme {
    let (name, t) = if dark { (DARK_NAME, &DARK) } else { (LIGHT_NAME, &LIGHT) };
    Theme::custom(
        name,
        Palette {
            background: t.background,
            text: t.text,
            primary: t.accent,
            success: t.success,
            warning: t.accent,
            danger: t.danger,
        },
    )
}

fn luminance(c: Color) -> f32 {
    let lin = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// WCAG 2.1 contrast ratio between two colours (1.0 to 21.0).
pub fn contrast(a: Color, b: Color) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_readable_in_both_themes() {
        for t in [&DARK, &LIGHT] {
            for bg in [t.background, t.pane, t.selection] {
                assert!(contrast(t.text, bg) >= 7.0);
                assert!(contrast(t.body, bg) >= 7.0);
                assert!(contrast(t.muted, bg) >= 4.5);
                assert!(contrast(t.faint, bg) >= 3.0);
            }
            assert!(contrast(t.accent, t.background) >= 3.0);
            assert!(contrast(t.success, t.background) >= 3.0);
            assert!(contrast(t.danger, t.background) >= 3.0);
        }
    }

    #[test]
    fn themes_round_trip_to_tokens() {
        assert_eq!(tokens(&theme(true)).background, DARK.background);
        assert_eq!(tokens(&theme(false)).background, LIGHT.background);
        assert_eq!(theme(true).palette().primary, DARK.accent);
    }
}

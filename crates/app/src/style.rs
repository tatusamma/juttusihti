//! Calm "warm paper & amber" design tokens and the custom Iced themes built from them.
// Consumed by later restyle tasks; remove once every token is used.
#![allow(dead_code)]

use iced::theme::Palette;
use iced::widget::{Space, button, container, mouse_area, opaque, pin, row as ui_row, stack, text};
use iced::{Background, Border, Color, Element, Fill, Point, Shadow, Theme, Vector, border};

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
    if theme.palette().background == LIGHT.background { &LIGHT } else { &DARK }
}

/// Muted category colours for the Map, Relationship map, Timeline and report bars.
/// Used only inside visualisations, never for UI chrome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DataColors {
    pub amber: Color,
    pub sage: Color,
    pub clay: Color,
    pub slate: Color,
    pub bar: Color,
}

const DATA_DARK: DataColors = DataColors {
    amber: hex(0xc9a063),
    sage: hex(0x8aa27a),
    clay: hex(0xc7745f),
    slate: hex(0x7f9bb0),
    bar: hex(0x9a7a4c),
};

const DATA_LIGHT: DataColors = DataColors {
    amber: hex(0xa8742f),
    sage: hex(0x5f7a3e),
    clay: hex(0xb04a38),
    slate: hex(0x4f6f8a),
    bar: hex(0xb08850),
};

/// The visualisation category colours matching an Iced theme.
pub fn data_colors(theme: &Theme) -> DataColors {
    if tokens(theme).background == LIGHT.background { DATA_LIGHT } else { DATA_DARK }
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

/// Article title size.
pub const SIZE_ARTICLE_TITLE: f32 = 24.0;
/// Screen title size.
pub const SIZE_TITLE: f32 = 17.0;
/// Body and list headline size.
pub const SIZE_BODY: f32 = 15.0;
/// Secondary text size; dimmer rather than smaller.
pub const SIZE_SMALL: f32 = 12.5;
/// Maximum article text width.
pub const READ_WIDTH: f32 = 680.0;
/// Spacing scale.
pub const SPACE: [f32; 5] = [4.0, 8.0, 12.0, 16.0, 24.0];

const RADIUS: f32 = 6.0;

fn darken(c: Color, amount: f32) -> Color {
    Color { r: c.r * (1.0 - amount), g: c.g * (1.0 - amount), b: c.b * (1.0 - amount), a: c.a }
}

fn fill(c: Color) -> Option<Background> {
    Some(Background::Color(c))
}

fn base(text_color: Color, background: Option<Background>, border: Border) -> button::Style {
    button::Style { background, text_color, border, shadow: Shadow::default(), snap: false }
}

/// The one amber-filled button of a screen.
pub fn main(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let border = border::rounded(RADIUS);
    match status {
        button::Status::Active => base(t.background, fill(t.accent), border),
        button::Status::Hovered | button::Status::Pressed => base(t.background, fill(darken(t.accent, 0.08)), border),
        button::Status::Disabled => base(t.faint, None, border),
    }
}

fn outlined(theme: &Theme, status: button::Status, text_color: Color) -> button::Style {
    let t = tokens(theme);
    let border = border::rounded(RADIUS).width(1.0).color(t.line);
    match status {
        button::Status::Active => base(text_color, None, border),
        button::Status::Hovered | button::Status::Pressed => base(text_color, fill(t.selection), border),
        button::Status::Disabled => base(t.faint, None, border),
    }
}

/// Outlined secondary button.
pub fn quiet(theme: &Theme, status: button::Status) -> button::Style {
    outlined(theme, status, tokens(theme).body)
}

/// Outlined button in its on/saved state: accent text.
pub fn quiet_active(theme: &Theme, status: button::Status) -> button::Style {
    outlined(theme, status, tokens(theme).accent)
}

/// Outlined button for destructive actions.
pub fn quiet_danger(theme: &Theme, status: button::Status) -> button::Style {
    outlined(theme, status, tokens(theme).danger)
}

fn bare(theme: &Theme, status: button::Status, text_color: Color) -> button::Style {
    let t = tokens(theme);
    let border = border::rounded(RADIUS);
    match status {
        button::Status::Active => base(text_color, None, border),
        button::Status::Hovered | button::Status::Pressed => base(text_color, fill(t.selection), border),
        button::Status::Disabled => base(t.faint, None, border),
    }
}

/// Borderless icon button.
pub fn icon(theme: &Theme, status: button::Status) -> button::Style {
    bare(theme, status, tokens(theme).muted)
}

/// Icon button in its on/saved state.
pub fn icon_active(theme: &Theme, status: button::Status) -> button::Style {
    bare(theme, status, tokens(theme).accent)
}

/// Unselected list row.
pub fn row(theme: &Theme, status: button::Status) -> button::Style {
    bare(theme, status, tokens(theme).text)
}

/// Selected list row.
pub fn row_selected(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let disabled = status == button::Status::Disabled;
    base(if disabled { t.faint } else { t.text }, fill(t.selection), border::rounded(RADIUS))
}

/// Plain pane surface.
pub fn pane(theme: &Theme) -> container::Style {
    container::Style { background: fill(tokens(theme).pane), ..container::Style::default() }
}

/// Floating surface: pane with a line border, rounded corners and a soft shadow.
pub fn popover(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        border: border::rounded(10.0).width(1.0).color(t.line),
        shadow: Shadow { color: Color { a: 0.25, ..Color::BLACK }, offset: Vector::new(0.0, 4.0), blur_radius: 16.0 },
        ..pane(theme)
    }
}

/// The thin accent edge marking the selected list row.
pub fn accent_edge(theme: &Theme) -> container::Style {
    container::Style {
        background: fill(tokens(theme).accent),
        border: border::rounded(1.0),
        ..container::Style::default()
    }
}

/// The small accent dot marking an unread article.
pub fn unread_dot(theme: &Theme) -> container::Style {
    container::Style {
        background: fill(tokens(theme).accent),
        border: border::rounded(3.0),
        ..container::Style::default()
    }
}

/// Status bar surface.
pub fn status(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style { text_color: Some(t.muted), border: border::width(1.0).color(t.line), ..pane(theme) }
}

/// Small tag/chip surface.
pub fn chip(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        text_color: Some(t.muted),
        background: fill(t.selection),
        border: border::rounded(RADIUS),
        ..container::Style::default()
    }
}

fn colored(color: Color) -> text::Style {
    text::Style { color: Some(color) }
}

/// Secondary text.
pub fn muted(theme: &Theme) -> text::Style {
    colored(tokens(theme).muted)
}

/// Tertiary text.
pub fn faint(theme: &Theme) -> text::Style {
    colored(tokens(theme).faint)
}

/// Reading text.
pub fn body(theme: &Theme) -> text::Style {
    colored(tokens(theme).body)
}

/// Accent-coloured text.
pub fn accent(theme: &Theme) -> text::Style {
    colored(tokens(theme).accent)
}

/// A row of joined quiet buttons; the selected one has the selection background and accent text.
pub fn segmented<'a, T: Copy + PartialEq + 'a, M: Clone + 'a>(
    options: &[(T, &'a str)],
    selected: T,
    on: impl Fn(T) -> M,
) -> Element<'a, M> {
    let last = options.len().saturating_sub(1);
    let buttons = options.iter().enumerate().map(|(i, &(value, label))| {
        let is_selected = value == selected;
        let radius = border::Radius {
            top_left: if i == 0 { RADIUS } else { 0.0 },
            bottom_left: if i == 0 { RADIUS } else { 0.0 },
            top_right: if i == last { RADIUS } else { 0.0 },
            bottom_right: if i == last { RADIUS } else { 0.0 },
        };
        button::Button::new(text(label).size(SIZE_SMALL))
            .padding([4.0, 12.0])
            .on_press(on(value))
            .style(move |theme: &Theme, status| {
                let t = tokens(theme);
                let mut style = quiet(theme, status);
                style.border = Border { radius, ..style.border };
                if is_selected {
                    style.background = fill(t.selection);
                    style.text_color = t.accent;
                }
                style
            })
            .into()
    });
    ui_row(buttons).spacing(-1.0).into()
}

/// Tooltip content: one line of small text on a popover surface.
pub fn tip<'a, M: 'a>(label: impl text::IntoFragment<'a>) -> Element<'a, M> {
    container(text(label).size(SIZE_SMALL)).padding([SPACE[0], SPACE[1]]).style(popover).into()
}

/// Floats `panel` over `base` with its top-left corner at `anchor`, measured from `base`'s top-left.
///
/// A transparent backdrop covers all of `base` while the panel is shown: pressing anywhere outside
/// the panel emits `on_close` and does not reach `base`. Presses inside the panel stay in the panel.
/// Give `panel` its own surface (usually [`popover`]) and size; the result takes `base`'s size.
pub fn overlay<'a, M: Clone + 'a>(
    base: impl Into<Element<'a, M>>,
    panel: impl Into<Element<'a, M>>,
    anchor: Point,
    on_close: M,
) -> Element<'a, M> {
    let backdrop = mouse_area(Space::new().width(Fill).height(Fill)).on_press(on_close);
    stack![base.into(), opaque(backdrop), pin(opaque(panel)).position(anchor)].into()
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
    fn data_colours_are_distinct_and_visible() {
        for (dark, t) in [(true, &DARK), (false, &LIGHT)] {
            let d = data_colors(&theme(dark));
            let all = [d.amber, d.sage, d.clay, d.slate];
            for (i, a) in all.iter().enumerate() {
                assert!(contrast(*a, t.background) >= 3.0, "{a:?} on {:?}", t.background);
                for b in &all[i + 1..] {
                    let dist = ((a.r - b.r).powi(2) + (a.g - b.g).powi(2) + (a.b - b.b).powi(2)).sqrt();
                    assert!(dist > 0.15, "{a:?} vs {b:?}: {dist}");
                }
            }
            assert!(contrast(d.bar, t.background) >= 3.0);
        }
    }

    #[test]
    fn themes_round_trip_to_tokens() {
        assert_eq!(tokens(&theme(true)).background, DARK.background);
        assert_eq!(tokens(&theme(false)).background, LIGHT.background);
        assert_eq!(theme(true).palette().primary, DARK.accent);
    }

    #[test]
    fn only_main_buttons_fill_with_accent() {
        let th = theme(true);
        let s = |f: fn(&Theme, button::Status) -> button::Style| f(&th, button::Status::Active);
        assert_eq!(s(main).background, Some(DARK.accent.into()));
        for f in [quiet, quiet_active, quiet_danger, icon, row] {
            assert_ne!(s(f).background, Some(DARK.accent.into()));
        }
        assert_eq!(s(quiet).border.width, 1.0);
    }

    #[test]
    fn text_sizes_are_four_and_readable() {
        let sizes = [SIZE_ARTICLE_TITLE, SIZE_TITLE, SIZE_BODY, SIZE_SMALL];
        assert!(sizes.iter().all(|s| *s >= 12.0));
    }
}

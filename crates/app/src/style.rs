//! Calm "warm paper & amber" design tokens, the custom Iced themes built from them,
//! and the shared text, button and container styles of every screen.

use iced::theme::Palette;
use iced::widget::overlay::menu;
use iced::widget::{
    Space, Text, button, checkbox as cb, container, mouse_area, opaque, pin, radio as rd, row as ui_row, stack, text,
    tooltip,
};
use iced::widget::{pick_list as pl, text_input as ti};
use iced::{Background, Border, Center, Color, Element, Fill, Font, Point, Shadow, Theme, Vector, border, font};

use crate::icons::{self, Icon};

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

/// The one amber-filled button of a screen. Disabled, it keeps a faint outline so it still reads as a button.
pub fn main(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let border = border::rounded(RADIUS);
    match status {
        button::Status::Active => base(t.background, fill(t.accent), border),
        button::Status::Hovered | button::Status::Pressed => base(t.background, fill(darken(t.accent, 0.08)), border),
        button::Status::Disabled => base(t.faint, None, border.width(1.0).color(t.line)),
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

/// Status bar surface: pane colour, muted text. The app draws a rule above it.
pub fn status(theme: &Theme) -> container::Style {
    container::Style { text_color: Some(tokens(theme).muted), ..pane(theme) }
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

/// Error text.
pub fn danger(theme: &Theme) -> text::Style {
    colored(tokens(theme).danger)
}

/// Bordered card for grouped items (review items, the selected map place, notes).
pub fn card(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style { border: border::rounded(RADIUS).width(1.0).color(t.line), ..container::Style::default() }
}

/// Neutral text field: `line` border on the pane colour, `muted` border on hover, accent while focused.
pub fn text_input(theme: &Theme, status: ti::Status) -> ti::Style {
    let t = tokens(theme);
    let edge = match status {
        ti::Status::Active | ti::Status::Disabled => t.line,
        ti::Status::Hovered => t.muted,
        ti::Status::Focused { .. } => t.accent,
    };
    let disabled = status == ti::Status::Disabled;
    ti::Style {
        background: Background::Color(t.pane),
        border: border::rounded(RADIUS).width(1.0).color(edge),
        icon: t.muted,
        placeholder: t.faint,
        value: if disabled { t.faint } else { t.text },
        selection: Color { a: 0.35, ..t.accent },
    }
}

/// Neutral drop-down: like [`text_input`], with the accent border while its menu is open.
pub fn pick_list(theme: &Theme, status: pl::Status) -> pl::Style {
    let t = tokens(theme);
    let edge = match status {
        pl::Status::Active => t.line,
        pl::Status::Hovered => t.muted,
        pl::Status::Opened { .. } => t.accent,
    };
    pl::Style {
        text_color: t.text,
        placeholder_color: t.faint,
        handle_color: t.muted,
        background: Background::Color(t.pane),
        border: border::rounded(RADIUS).width(1.0).color(edge),
    }
}

/// The open menu of a [`pick_list`]: popover surface, the hovered entry on the selection colour.
pub fn menu(theme: &Theme) -> menu::Style {
    let t = tokens(theme);
    menu::Style {
        background: Background::Color(t.pane),
        border: border::rounded(RADIUS).width(1.0).color(t.line),
        text_color: t.body,
        selected_text_color: t.text,
        selected_background: Background::Color(t.selection),
        shadow: Shadow::default(),
    }
}

/// Neutral checkbox: text-coloured check on the pane colour with a border; never amber.
pub fn checkbox(theme: &Theme, status: cb::Status) -> cb::Style {
    let t = tokens(theme);
    let (hovered, disabled) = match status {
        cb::Status::Active { .. } => (false, false),
        cb::Status::Hovered { .. } => (true, false),
        cb::Status::Disabled { .. } => (false, true),
    };
    cb::Style {
        background: Background::Color(if hovered { t.selection } else { t.pane }),
        icon_color: if disabled { t.faint } else { t.text },
        border: border::rounded(3.0).width(1.0).color(if disabled { t.line } else { t.muted }),
        text_color: Some(if disabled { t.faint } else { t.body }),
    }
}

/// Neutral radio button: text-coloured dot on the pane colour with a border; never amber.
pub fn radio(theme: &Theme, status: rd::Status) -> rd::Style {
    let t = tokens(theme);
    let hovered = matches!(status, rd::Status::Hovered { .. });
    rd::Style {
        background: Background::Color(if hovered { t.selection } else { t.pane }),
        dot_color: t.text,
        border_width: 1.0,
        border_color: t.muted,
        text_color: Some(t.body),
    }
}

/// A labelled checkbox in the neutral [`checkbox`] style.
pub fn check<'a, M: 'a>(on: bool, label: impl text::IntoFragment<'a>) -> iced::widget::Checkbox<'a, M> {
    cb(on).label(label).size(SIZE_BODY).text_size(SIZE_BODY).style(checkbox)
}

/// A radio button in the neutral [`radio`] style.
pub fn choice<'a, M: Clone + 'a, V: Copy + Eq>(
    label: impl Into<String>,
    value: V,
    selected: Option<V>,
    on: impl FnOnce(V) -> M,
) -> iced::widget::Radio<'a, M> {
    rd(label, value, selected, on).size(SIZE_BODY).text_size(SIZE_BODY).style(radio)
}

/// Semibold, for titles and headings.
pub const SEMIBOLD: Font = Font { weight: font::Weight::Semibold, ..Font::DEFAULT };

/// A screen title.
pub fn title<'a>(label: impl text::IntoFragment<'a>) -> Text<'a> {
    text(label).size(SIZE_TITLE).font(SEMIBOLD)
}

/// A section heading inside a screen.
pub fn heading<'a>(label: impl text::IntoFragment<'a>) -> Text<'a> {
    text(label).size(SIZE_BODY).font(SEMIBOLD)
}

/// A line of secondary text.
pub fn note<'a>(label: impl text::IntoFragment<'a>) -> Text<'a> {
    text(label).size(SIZE_SMALL).style(muted)
}

/// Width of the help tooltip before its text wraps.
const HELP_WIDTH: f32 = 380.0;

/// A small "?" whose tooltip holds a full explanation.
pub fn help<'a, M: 'a>(full: impl text::IntoFragment<'a>) -> Element<'a, M> {
    let tip = container(text(full).size(SIZE_SMALL)).padding([SPACE[1], SPACE[2]]).max_width(HELP_WIDTH).style(popover);
    tooltip(icons::icon(Icon::Help).size(SIZE_BODY).style(faint), tip, tooltip::Position::Bottom).gap(SPACE[0]).into()
}

/// One muted line followed by a "?" with the full explanation.
pub fn intro<'a, M: 'a>(line: impl text::IntoFragment<'a>, full: impl text::IntoFragment<'a>) -> Element<'a, M> {
    iced::widget::row![note(line), help(full)].spacing(SPACE[1]).align_y(Center).into()
}

/// A done/failed message: body text, or danger text for an error.
pub fn message<'a, M: 'a>(m: &'a Result<String, String>) -> Element<'a, M> {
    match m {
        Ok(m) => text(m).size(SIZE_SMALL).style(body).into(),
        Err(e) => text(e).size(SIZE_SMALL).style(danger).into(),
    }
}

/// A quiet button with small text, optionally led by an icon in the same colour.
/// `style` is [`quiet`], [`quiet_active`] or [`quiet_danger`]; `None` disables it.
pub fn quiet_button<'a, M: Clone + 'a>(
    lead: Option<Icon>,
    label: impl text::IntoFragment<'a>,
    style: fn(&Theme, button::Status) -> button::Style,
    on_press: Option<M>,
) -> Element<'a, M> {
    let mut content = iced::widget::row![].spacing(SPACE[0] + 2.0).align_y(Center);
    if let Some(i) = lead {
        content = content.push(icons::icon(i).size(SIZE_SMALL));
    }
    content = content.push(text(label).size(SIZE_SMALL));
    button::Button::new(content).padding([SPACE[0], SPACE[1] + 2.0]).style(style).on_press_maybe(on_press).into()
}

/// The one amber main button of a screen; `None` disables it.
pub fn main_button<'a, M: Clone + 'a>(label: impl text::IntoFragment<'a>, on_press: Option<M>) -> Element<'a, M> {
    button::Button::new(text(label).size(SIZE_BODY))
        .padding([SPACE[1], SPACE[2]])
        .style(main)
        .on_press_maybe(on_press)
        .into()
}

/// A borderless icon button with a tooltip; `None` disables it.
pub fn icon_button<'a, M: Clone + 'a>(i: Icon, tip_text: &'a str, size: f32, on_press: Option<M>) -> Element<'a, M> {
    tooltip(
        button::Button::new(icons::icon(i).size(size)).padding(SPACE[0]).style(self::icon).on_press_maybe(on_press),
        tip(tip_text),
        tooltip::Position::Top,
    )
    .into()
}

/// A row of joined quiet buttons; the selected one has the selection background and accent text.
pub fn segmented<'a, T: Copy + PartialEq + 'a, M: Clone + 'a, L: text::IntoFragment<'a>>(
    options: impl IntoIterator<Item = (T, L)>,
    selected: T,
    on: impl Fn(T) -> M,
) -> Element<'a, M> {
    let options: Vec<(T, L)> = options.into_iter().collect();
    let last = options.len().saturating_sub(1);
    let buttons = options.into_iter().enumerate().map(|(i, (value, label))| {
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

// Only the contrast tests use these.
#[cfg_attr(not(test), allow(dead_code))]
fn luminance(c: Color) -> f32 {
    let lin = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// WCAG 2.1 contrast ratio between two colours (1.0 to 21.0).
#[cfg_attr(not(test), allow(dead_code))]
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
        assert_eq!(main(&th, button::Status::Disabled).border.width, 1.0);
    }

    #[test]
    fn no_literal_text_sizes_in_views() {
        let sources = [
            ("app.rs", include_str!("app.rs")),
            ("reports.rs", include_str!("reports.rs")),
            ("research.rs", include_str!("research.rs")),
            ("review.rs", include_str!("review.rs")),
            ("labels.rs", include_str!("labels.rs")),
            ("providers.rs", include_str!("providers.rs")),
            ("settings.rs", include_str!("settings.rs")),
            ("timeline_view.rs", include_str!("timeline_view.rs")),
            ("graph_view.rs", include_str!("graph_view.rs")),
            ("map_view.rs", include_str!("map_view.rs")),
            ("quick.rs", include_str!("quick.rs")),
            ("rail.rs", include_str!("rail.rs")),
            ("feeds_menu.rs", include_str!("feeds_menu.rs")),
        ];
        // Widget sizes and canvas text sizes alike: a digit right after the opener is a literal.
        let openers = [".size(", ".text_size(", "size: "];
        for (name, src) in sources {
            for (n, line) in src.lines().enumerate() {
                for opener in openers {
                    let literal = line
                        .match_indices(opener)
                        .any(|(i, _)| line[i + opener.len()..].starts_with(|c: char| c.is_ascii_digit()));
                    assert!(!literal, "{name}:{}: literal text size; use style::SIZE_* constants", n + 1);
                }
            }
        }
    }

    #[test]
    fn text_sizes_are_four_and_readable() {
        let sizes = [SIZE_ARTICLE_TITLE, SIZE_TITLE, SIZE_BODY, SIZE_SMALL];
        assert!(sizes.iter().all(|s| *s >= 12.0));
    }
}

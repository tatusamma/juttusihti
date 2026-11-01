//! The narrow icon rail on the left: reading places, research tools and setup.

use iced::widget::{Space, button, column, container, tooltip};
use iced::{Center, Element, Fill};

use crate::app::{Message, Place, Screen};
use crate::icons::{Icon, icon};
use crate::style;

/// Rail width in pixels.
pub const WIDTH: f32 = 52.0;

struct Item {
    icon: Icon,
    name: &'static str,
    target: Message,
    current: bool,
}

pub fn view<'a>(place: Place, screen: Screen) -> Element<'a, Message> {
    let reading = |p: Place| Item {
        icon: if p == Place::News { Icon::News } else { Icon::Saved },
        name: if p == Place::News { "News" } else { "Saved" },
        target: Message::GoPlace(p),
        current: screen == Screen::Reader && place == p,
    };
    let tool = |icon: Icon, name: &'static str, s: Screen| Item {
        icon,
        name,
        target: Message::ShowScreen(s),
        current: screen == s,
    };
    let top = [
        reading(Place::News),
        reading(Place::Saved),
        tool(Icon::Reports, "Reports", Screen::Reports),
        tool(Icon::Review, "Review", Screen::Review),
        tool(Icon::Timeline, "Timeline", Screen::Timeline),
        tool(Icon::Map, "Map", Screen::Map),
        tool(Icon::Connections, "Relationship map", Screen::Graph),
        tool(Icon::Analysis, "Custom analysis", Screen::Research),
    ];
    let bottom = [
        tool(Icon::AiSetup, "AI setup", Screen::Providers),
        tool(Icon::Labels, "Labels", Screen::Labels),
        tool(Icon::Settings, "Settings", Screen::Settings),
        tool(Icon::Help, "Help (F1)", Screen::Help),
    ];
    container(
        column(top.into_iter().map(entry))
            .push(Space::new().height(Fill))
            .extend(bottom.into_iter().map(entry))
            .spacing(style::SPACE[0])
            .align_x(Center),
    )
    .width(WIDTH)
    .height(Fill)
    .padding([style::SPACE[1], 0.0])
    .style(style::pane)
    .into()
}

fn entry<'a>(item: Item) -> Element<'a, Message> {
    let b = button(icon(item.icon))
        .padding(style::SPACE[1])
        .style(if item.current { style::icon_active } else { style::icon })
        .on_press(item.target);
    tooltip(b, style::tip(item.name), tooltip::Position::Right).gap(style::SPACE[0]).into()
}

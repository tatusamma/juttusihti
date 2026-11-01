//! The feeds dropdown above the article list: choose all feeds, a folder or a feed,
//! edit or unsubscribe a feed, add a feed and import or export OPML.

use iced::widget::button::Button;
use iced::widget::{Space, checkbox, column, container, pick_list, row, rule, scrollable, text, text_input, tooltip};
use iced::{Center, Element, Fill, Padding};
use juttusihti_core::{Feed, FeedId};

use crate::app::{FeedEdit, FolderChoice, Message, Scope, Sidebar};
use crate::icons::{Icon, icon};
use crate::style::{self, SIZE_BODY, SIZE_SMALL, SPACE};

/// Panel width in pixels.
pub const WIDTH: f32 = 340.0;
/// Most height the feed list takes before it scrolls.
const LIST_MAX_HEIGHT: f32 = 420.0;
/// Least height the feed list keeps, so a few rows stay visible.
const LIST_MIN_HEIGHT: f32 = 80.0;
/// Height of the Add / Import / Export block, its rule and the panel padding.
const FOOTER_HEIGHT: f32 = 130.0;
/// Height of the pinned edit form and of the pinned unsubscribe question.
const EDIT_HEIGHT: f32 = 245.0;
const CONFIRM_HEIGHT: f32 = 90.0;

/// Height for the scrolling feed list when `available` pixels are free below the list
/// header, so the panel (with the pinned form and the footer) always fits the window.
pub fn list_max_height(available: f32, editing: bool, confirming: bool) -> f32 {
    let mut pinned = 0.0;
    if editing {
        pinned += EDIT_HEIGHT;
    }
    if confirming {
        pinned += CONFIRM_HEIGHT;
    }
    (available - FOOTER_HEIGHT - pinned).clamp(LIST_MIN_HEIGHT, LIST_MAX_HEIGHT)
}
/// Longest scope name the dropdown button shows before cutting it with "…".
const BUTTON_MAX_CHARS: usize = 28;

/// The dropdown button: current scope, its unread count and a chevron.
pub fn button<'a>(label: String, unread: u64, open: bool) -> Element<'a, Message> {
    let mut content = row![text(shorten(&label, BUTTON_MAX_CHARS)).size(SIZE_BODY)].spacing(SPACE[1]).align_y(Center);
    if unread > 0 {
        content = content.push(text(unread.to_string()).size(SIZE_SMALL).style(style::faint));
    }
    content = content.push(icon(Icon::ChevronDown).size(SIZE_SMALL).style(style::muted));
    Button::new(content)
        .padding([SPACE[0], SPACE[1]])
        .style(if open { style::row_selected } else { style::row })
        .on_press(Message::ToggleFeedsMenu)
        .into()
}

/// `label` cut to at most `max` characters, ending in "…" when cut.
fn shorten(label: &str, max: usize) -> String {
    if label.chars().count() <= max {
        return label.to_string();
    }
    let mut cut: String = label.chars().take(max - 1).collect();
    cut.push('…');
    cut
}

/// The dropdown panel. `edit` is the feed whose edit form is open; `confirm` the feed
/// waiting for an "Unsubscribe from …?" answer (both pinned above the list); `can_add` is
/// false without networking; `list_height` caps the scrolling list (see [`list_max_height`]).
pub fn panel<'a>(
    sidebar: &'a Sidebar,
    scope: Scope,
    add_feed: &'a str,
    edit: Option<&'a FeedEdit>,
    confirm: Option<FeedId>,
    can_add: bool,
    list_height: f32,
) -> Element<'a, Message> {
    let unread_of = |id: FeedId| sidebar.unread.iter().find(|(f, _)| *f == id).map_or(0, |(_, n)| *n);
    let feed_entry = |f: &'a Feed, indent: bool| -> Element<'a, Message> {
        let label = if f.last_error.is_some() { format!("⚠ {}", f.title) } else { f.title.clone() };
        let edit_button = tooltip(
            Button::new(icon(Icon::Edit).size(SIZE_SMALL))
                .padding(SPACE[0])
                .style(style::icon)
                .on_press(Message::EditFeed(f.id)),
            style::tip("Edit feed"),
            tooltip::Position::Left,
        );
        row![
            scope_row(label, unread_of(f.id), scope == Scope::Feed(f.id), indent, Message::SetScope(Scope::Feed(f.id))),
            edit_button,
        ]
        .spacing(SPACE[0])
        .align_y(Center)
        .into()
    };

    let total: u64 = sidebar.unread.iter().map(|(_, n)| n).sum();
    let mut list =
        column![scope_row("All feeds".into(), total, scope == Scope::All, false, Message::SetScope(Scope::All))]
            .spacing(2);
    for folder in &sidebar.folders {
        let feeds: Vec<&Feed> = sidebar.feeds.iter().filter(|f| f.folder_id == Some(folder.id)).collect();
        let unread = feeds.iter().map(|f| unread_of(f.id)).sum();
        list = list.push(scope_row(
            folder.name.clone(),
            unread,
            scope == Scope::Folder(folder.id),
            false,
            Message::SetScope(Scope::Folder(folder.id)),
        ));
        for f in feeds {
            list = list.push(feed_entry(f, true));
        }
    }
    for f in sidebar.feeds.iter().filter(|f| f.folder_id.is_none()) {
        list = list.push(feed_entry(f, false));
    }
    if sidebar.feeds.is_empty() {
        list = list.push(container(text("No feeds yet.").size(SIZE_SMALL).style(style::faint)).padding(SPACE[1]));
    }

    let add = column![
        row![
            text_input("Add a site or feed", add_feed)
                .style(style::text_input)
                .on_input(Message::AddFeedInput)
                .on_submit(Message::AddFeed)
                .size(SIZE_BODY)
                .padding(SPACE[1]),
            style::main_button("Add", can_add.then_some(Message::AddFeed)),
        ]
        .spacing(SPACE[1])
        .align_y(Center),
        row![quiet("Import OPML", Message::ImportOpml), quiet("Export OPML", Message::ExportOpml)].spacing(SPACE[1]),
    ]
    .spacing(SPACE[1]);

    // The edit form and the unsubscribe question sit above the list, always in view.
    let mut body = column![].spacing(SPACE[1]);
    if let Some((e, f)) = edit.and_then(|e| sidebar.feeds.iter().find(|f| f.id == e.id).map(|f| (e, f))) {
        body = body.push(edit_form(sidebar, f, e));
    }
    if let Some(f) = confirm.and_then(|id| sidebar.feeds.iter().find(|f| f.id == id)) {
        body = body.push(confirm_unsubscribe(f));
    }
    container(
        body.push(
            container(scrollable(list.padding(Padding { right: SPACE[2], ..Padding::ZERO }))).max_height(list_height),
        )
        .push(rule::horizontal(1))
        .push(add),
    )
    .width(WIDTH)
    .padding(SPACE[1])
    .style(style::popover)
    .into()
}

/// One choosable scope with its unread count.
fn scope_row<'a>(label: String, unread: u64, selected: bool, indent: bool, target: Message) -> Element<'a, Message> {
    let mut content = row![text(label).size(SIZE_BODY).width(Fill)].spacing(SPACE[1]).align_y(Center);
    if unread > 0 {
        content = content.push(text(unread.to_string()).size(SIZE_SMALL).style(style::faint));
    }
    let left = if indent { SPACE[3] + SPACE[1] } else { SPACE[1] };
    Button::new(content)
        .width(Fill)
        .padding(Padding { top: SPACE[0] + 2.0, bottom: SPACE[0] + 2.0, left, right: SPACE[1] })
        .style(if selected { style::row_selected } else { style::row })
        .on_press(target)
        .into()
}

fn quiet<'a>(label: &'a str, target: Message) -> Element<'a, Message> {
    style::quiet_button(None, label, style::quiet, Some(target))
}

/// Title, folder, full-article text and Unsubscribe for one feed, pinned above the list.
fn edit_form<'a>(sidebar: &'a Sidebar, feed: &'a Feed, edit: &'a FeedEdit) -> Element<'a, Message> {
    let mut choices = vec![FolderChoice(None, "No folder".into())];
    choices.extend(sidebar.folders.iter().map(|f| FolderChoice(Some(f.id), f.name.clone())));
    let selected = choices.iter().find(|c| c.0 == edit.folder).cloned();
    container(
        column![
            text(&feed.url).size(SIZE_SMALL).style(style::faint),
            text_input("Title", &edit.title)
                .style(style::text_input)
                .on_input(Message::FeedTitle)
                .on_submit(Message::SaveFeed)
                .size(SIZE_BODY)
                .padding(SPACE[1]),
            row![
                pick_list(choices, selected, Message::FeedFolder)
                    .style(style::pick_list)
                    .menu_style(style::menu)
                    .text_size(SIZE_SMALL)
                    .width(Fill),
                text_input("or new folder", &edit.new_folder)
                    .style(style::text_input)
                    .on_input(Message::NewFolder)
                    .size(SIZE_SMALL)
                    .padding(SPACE[1])
                    .width(Fill),
            ]
            .spacing(SPACE[1])
            .align_y(Center),
            tooltip(
                checkbox(edit.full_text)
                    .label("Fetch full articles for short entries")
                    .size(SIZE_SMALL)
                    .text_size(SIZE_SMALL)
                    .style(style::checkbox)
                    .on_toggle(Message::FeedFullText),
                style::tip(
                    "When an entry only has a teaser, load the article page and keep its main text \
                     for reading, search and AI analysis. No JavaScript is run."
                ),
                tooltip::Position::Bottom,
            ),
            row![
                quiet("Save", Message::SaveFeed),
                quiet("Cancel", Message::CancelEditFeed),
                Space::new().width(Fill),
                style::quiet_button(None, "Unsubscribe", style::quiet_danger, Some(Message::RemoveFeed(feed.id))),
            ]
            .spacing(SPACE[1]),
        ]
        .spacing(SPACE[1]),
    )
    .padding([SPACE[1], SPACE[1]])
    .into()
}

/// The one-time question before a feed is removed.
fn confirm_unsubscribe<'a>(feed: &'a Feed) -> Element<'a, Message> {
    container(
        column![
            text(format!("Unsubscribe from {}?", feed.title)).size(SIZE_SMALL),
            row![
                style::quiet_button(None, "Unsubscribe", style::quiet_danger, Some(Message::ConfirmUnsubscribe)),
                quiet("Cancel", Message::CancelUnsubscribe),
            ]
            .spacing(SPACE[1]),
        ]
        .spacing(SPACE[1]),
    )
    .padding([SPACE[1], SPACE[1]])
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_height_leaves_room_for_the_footer_and_pinned_blocks() {
        assert_eq!(list_max_height(2000.0, false, false), LIST_MAX_HEIGHT);
        // 560px window: 97px header leaves 463; the footer takes its share.
        assert_eq!(list_max_height(463.0, false, false), 463.0 - FOOTER_HEIGHT);
        assert_eq!(list_max_height(463.0, true, false), 463.0 - FOOTER_HEIGHT - EDIT_HEIGHT);
        assert_eq!(list_max_height(463.0, false, true), 463.0 - FOOTER_HEIGHT - CONFIRM_HEIGHT);
        assert_eq!(list_max_height(100.0, true, true), LIST_MIN_HEIGHT);
    }

    #[test]
    fn long_scope_names_are_shortened() {
        assert_eq!(shorten("Yle Uutiset", 28), "Yle Uutiset");
        assert_eq!(shorten("Helsingin Sanomat – Kotimaa", 10), "Helsingin…");
    }
}

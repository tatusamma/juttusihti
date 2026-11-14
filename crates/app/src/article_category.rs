//! The article view's quiet "Category" row: the effective category names, where
//! they came from, and a pick list to set one by hand or go back to Automatic.

use iced::widget::{pick_list, row, text};
use iced::{Center, Element};
use juttusihti_core::categories::{ArticleCategory, Category, CategoryId};

use crate::style::{self, SIZE_BODY, SIZE_SMALL, SPACE};

/// An entry in the pick list: a category, or `None` for "Automatic".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice(pub Option<CategoryId>, pub String);

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}

/// The effective category names ("None" when there are none) and a muted note
/// on where they came from.
pub fn describe(c: &ArticleCategory) -> (String, Option<String>) {
    if let Some(m) = &c.manual {
        return (m.name.clone(), Some("set by you".into()));
    }
    if let Some(f) = &c.from_feed {
        return (f.category.name.clone(), Some(format!("default of feed {}", f.feed_title)));
    }
    if !c.from_terms.is_empty() {
        let names: Vec<&str> = c.from_terms.iter().map(|t| t.name.as_str()).collect();
        return (names.join(", "), Some("from the feed".into()));
    }
    match &c.ai {
        Some(a) => (a.name.clone(), Some("from AI".into())),
        None => ("None".into(), None),
    }
}

/// The pick list's entries: "Automatic" first when a manual choice exists, then
/// every category alphabetically.
pub fn choices(all: &[Category], c: &ArticleCategory) -> Vec<Choice> {
    let mut sorted: Vec<&Category> = all.iter().collect();
    sorted.sort_by_key(|c| c.name.to_lowercase());
    let mut out = Vec::new();
    if c.manual.is_some() {
        out.push(Choice(None, "Automatic".into()));
    }
    out.extend(sorted.into_iter().map(|c| Choice(Some(c.id), c.name.clone())));
    out
}

pub fn view<'a, M: Clone + 'a>(
    all: &[Category],
    c: &ArticleCategory,
    on_pick: impl Fn(Option<CategoryId>) -> M + 'a,
) -> Element<'a, M> {
    let (names, origin) = describe(c);
    let mut line = row![text("Category").size(SIZE_SMALL).style(style::faint), text(names).size(SIZE_BODY)]
        .spacing(SPACE[1])
        .align_y(Center);
    if let Some(o) = origin {
        line = line.push(text(o).size(SIZE_SMALL).style(style::faint));
    }
    let list = choices(all, c);
    if !list.is_empty() {
        let picker = pick_list(list, None::<Choice>, move |ch: Choice| on_pick(ch.0))
            .placeholder("Change category…")
            .style(style::pick_list)
            .menu_style(style::menu)
            .text_size(SIZE_SMALL)
            .padding(SPACE[0]);
        line = line.push(picker);
    }
    line.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use juttusihti_core::categories::FeedDefault;

    fn cat(id: i64, name: &str) -> Category {
        Category { id, name: name.into() }
    }

    #[test]
    fn preview_shows_where_the_category_came_from() {
        let none = ArticleCategory::default();
        assert_eq!(describe(&none), ("None".into(), None));
        let feed = ArticleCategory { from_terms: vec![cat(1, "Sport")], ..Default::default() };
        assert_eq!(describe(&feed), ("Sport".into(), Some("from the feed".into())));
        let ai = ArticleCategory { ai: Some(cat(2, "Tech")), ..Default::default() };
        assert_eq!(describe(&ai), ("Tech".into(), Some("from AI".into())));
        let both = ArticleCategory {
            from_terms: vec![cat(1, "Sport")],
            ai: Some(cat(2, "Tech")),
            manual: None,
            from_feed: None,
        };
        let default = ArticleCategory {
            from_feed: Some(FeedDefault { category: cat(4, "Sport"), feed_title: "Yle Urheilu".into() }),
            ..Default::default()
        };
        assert_eq!(describe(&default), ("Sport".into(), Some("default of feed Yle Urheilu".into())));
        let manual = ArticleCategory { manual: Some(cat(3, "Local")), ..both };
        assert_eq!(describe(&manual), ("Local".into(), Some("set by you".into())));
    }

    #[test]
    fn automatic_is_offered_only_with_a_manual_choice() {
        let all = [cat(2, "tech"), cat(1, "Sport")];
        let auto = choices(&all, &ArticleCategory::default());
        assert_eq!(auto.iter().map(|c| c.1.as_str()).collect::<Vec<_>>(), ["Sport", "tech"]);
        let manual = ArticleCategory { manual: Some(cat(1, "Sport")), ..Default::default() };
        assert_eq!(choices(&all, &manual)[0], Choice(None, "Automatic".into()));
    }
}

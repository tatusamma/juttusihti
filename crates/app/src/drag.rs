//! Small reusable drag and drop for lists. Iced has no built-in drag and drop,
//! so a screen keeps a [`DragState`], wraps grips with [`handle`] and drop zones
//! with [`target`], includes [`subscription`] while a drag is armed, and draws
//! [`ghost`] in an overlay `stack!` at the cursor. Esc maps to [`DragMsg::Cancel`]
//! in the screen.

use iced::widget::{Space, container, mouse_area, row, text};
use iced::{Border, Element, Length, Point, Subscription, Theme, border, event, mouse};

use crate::style;

/// Pointer travel (px) below which a press-and-release is a click, not a drop.
pub const DRAG_THRESHOLD: f32 = 4.0;

const OUTLINE_RADIUS: f32 = 6.0;

/// What is being dragged and where it may land; generic over the screen's item and target types.
#[derive(Debug, Clone, PartialEq)]
pub struct Drag<I, T> {
    pub item: I,
    pub label: String,
    pub start: Point,
    pub cursor: Point,
    pub over: Option<T>,
    pub active: bool,
    /// Every target the pointer is inside, outermost first; `over` is the innermost.
    entered: Vec<T>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DragMsg<I, T> {
    /// Pressed on a grip. [`handle`] cannot know the pointer position, so it
    /// sends a NaN point and the first `Move` supplies the real start.
    Press(I, String, Point),
    Move(Point),
    Enter(T),
    Leave(T),
    Release,
    Cancel,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DragState<I, T>(Option<Drag<I, T>>);

impl<I, T> Default for DragState<I, T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<I: Clone, T: Clone + PartialEq> DragState<I, T> {
    /// Apply a message; returns `Some((item, target))` on a drop over a target.
    pub fn update(&mut self, msg: DragMsg<I, T>) -> Option<(I, T)> {
        match msg {
            DragMsg::Press(item, label, at) => {
                self.0 =
                    Some(Drag { item, label, start: at, cursor: at, over: None, active: false, entered: Vec::new() });
            }
            DragMsg::Move(p) => {
                if let Some(d) = &mut self.0 {
                    if d.start.x.is_nan() || d.start.y.is_nan() {
                        d.start = p;
                    }
                    d.cursor = p;
                    if !d.active && d.start.distance(p) >= DRAG_THRESHOLD {
                        d.active = true;
                    }
                }
            }
            DragMsg::Enter(t) => {
                if let Some(d) = &mut self.0 {
                    d.entered.retain(|e| *e != t);
                    d.entered.push(t);
                    d.over = d.entered.last().cloned();
                }
            }
            DragMsg::Leave(t) => {
                if let Some(d) = &mut self.0 {
                    d.entered.retain(|e| *e != t);
                    d.over = d.entered.last().cloned();
                }
            }
            DragMsg::Release => {
                let d = self.0.take()?;
                return if d.active { d.over.map(|t| (d.item, t)) } else { None };
            }
            DragMsg::Cancel => self.0 = None,
        }
        None
    }

    /// The drag in progress (armed or active), if any.
    pub fn dragging(&self) -> Option<&Drag<I, T>> {
        self.0.as_ref()
    }
}

/// Window-level pointer events for the duration of a drag: cursor moves and
/// left-button release. Include it only while [`DragState::dragging`] is `Some`.
/// (`on_release` is a function because Iced subscriptions cannot capture values.)
pub fn subscription<M: 'static + Send>(on_move: fn(Point) -> M, on_release: fn() -> M) -> Subscription<M> {
    enum Raw {
        Move(Point),
        Release,
    }
    event::listen_with(|e, _status, _window| match e {
        iced::Event::Mouse(mouse::Event::CursorMoved { position }) => Some(Raw::Move(position)),
        iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => Some(Raw::Release),
        _ => None,
    })
    .with((on_move, on_release))
    .map(|((on_move, on_release), raw)| match raw {
        Raw::Move(p) => on_move(p),
        Raw::Release => on_release(),
    })
}

/// A "⋮⋮" grip beside `content`; pressing it sends `on_press` (with a NaN point, see [`DragMsg::Press`]).
pub fn handle<'a, M: Clone + 'a>(content: Element<'a, M>, on_press: impl Fn(Point) -> M) -> Element<'a, M> {
    let grip = text("⋮⋮").size(style::SIZE_BODY).style(style::faint);
    mouse_area(row![grip, content].spacing(style::SPACE[1]).align_y(iced::Center))
        .on_press(on_press(Point::new(f32::NAN, f32::NAN)))
        .interaction(mouse::Interaction::Grab)
        .into()
}

/// A drop zone around `content`: an amber outline while `highlighted`.
pub fn target<'a, M: Clone + 'a>(
    content: Element<'a, M>,
    highlighted: bool,
    on_enter: M,
    on_exit: M,
) -> Element<'a, M> {
    let zone = container(content).style(move |theme: &Theme| container::Style {
        // Iced borders cannot be dashed; a solid amber outline stands in.
        border: Border {
            color: if highlighted { style::tokens(theme).accent } else { iced::Color::TRANSPARENT },
            width: 1.5,
            radius: border::radius(OUTLINE_RADIUS),
        },
        ..container::Style::default()
    });
    mouse_area(zone).on_enter(on_enter).on_exit(on_exit).into()
}

/// An insertion drop zone: a 2 px amber line above `content` while `highlighted`.
pub fn insertion<'a, M: Clone + 'a>(
    content: Element<'a, M>,
    highlighted: bool,
    on_enter: M,
    on_exit: M,
) -> Element<'a, M> {
    let line = container(Space::new().width(Length::Fill).height(2.0)).style(move |theme: &Theme| container::Style {
        background: highlighted.then(|| style::tokens(theme).accent.into()),
        ..container::Style::default()
    });
    mouse_area(iced::widget::column![line, content]).on_enter(on_enter).on_exit(on_exit).into()
}

/// The floating chip that follows the cursor; place it in an overlay `stack!`
/// (e.g. inside `pin(...).x(cursor.x).y(cursor.y)`).
pub fn ghost<'a, M: 'a>(label: &'a str) -> Element<'a, M> {
    container(text(label).size(style::SIZE_SMALL).style(style::body)).padding([4, 10]).style(style::popover).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    type S = DragState<u32, &'static str>;

    fn p(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    fn pressed() -> S {
        let mut s = S::default();
        s.update(DragMsg::Press(7, "Seven".into(), p(10.0, 10.0)));
        s
    }

    #[test]
    fn small_movement_is_a_click_not_a_drop() {
        let mut s = pressed();
        s.update(DragMsg::Move(p(12.0, 11.0)));
        s.update(DragMsg::Enter("a"));
        assert!(!s.dragging().unwrap().active);
        assert_eq!(s.update(DragMsg::Release), None);
        assert!(s.dragging().is_none());
    }

    #[test]
    fn drop_over_target_returns_item_and_target() {
        let mut s = pressed();
        s.update(DragMsg::Move(p(30.0, 10.0)));
        assert!(s.dragging().unwrap().active);
        s.update(DragMsg::Enter("a"));
        assert_eq!(s.update(DragMsg::Release), Some((7, "a")));
        assert!(s.dragging().is_none());
    }

    #[test]
    fn leaving_target_clears_it() {
        let mut s = pressed();
        s.update(DragMsg::Move(p(30.0, 10.0)));
        s.update(DragMsg::Enter("a"));
        s.update(DragMsg::Leave("b"));
        assert_eq!(s.dragging().unwrap().over, Some("a"));
        s.update(DragMsg::Leave("a"));
        assert_eq!(s.dragging().unwrap().over, None);
        assert_eq!(s.update(DragMsg::Release), None);
    }

    #[test]
    fn adjacent_targets_drop_on_the_latest() {
        let mut s = pressed();
        s.update(DragMsg::Move(p(30.0, 10.0)));
        s.update(DragMsg::Enter("a"));
        s.update(DragMsg::Enter("b"));
        s.update(DragMsg::Leave("a"));
        assert_eq!(s.update(DragMsg::Release), Some((7, "b")));
    }

    #[test]
    fn leaving_a_nested_target_falls_back_to_the_parent() {
        let mut s = pressed();
        s.update(DragMsg::Move(p(30.0, 10.0)));
        s.update(DragMsg::Enter("g"));
        s.update(DragMsg::Enter("c"));
        s.update(DragMsg::Leave("c"));
        assert_eq!(s.update(DragMsg::Release), Some((7, "g")));
    }

    #[test]
    fn movement_of_exactly_the_threshold_activates() {
        let mut s = pressed();
        s.update(DragMsg::Move(p(10.0 + DRAG_THRESHOLD, 10.0)));
        assert!(s.dragging().unwrap().active);
    }

    #[test]
    fn a_new_press_replaces_the_drag() {
        let mut s = pressed();
        s.update(DragMsg::Move(p(30.0, 10.0)));
        s.update(DragMsg::Enter("a"));
        s.update(DragMsg::Press(8, "Eight".into(), p(0.0, 0.0)));
        let d = s.dragging().unwrap();
        assert_eq!((d.item, d.active, d.over), (8, false, None));
    }

    #[test]
    fn cancel_drops_nothing() {
        let mut s = pressed();
        s.update(DragMsg::Move(p(30.0, 10.0)));
        s.update(DragMsg::Enter("a"));
        assert_eq!(s.update(DragMsg::Cancel), None);
        assert!(s.dragging().is_none());
        assert_eq!(s.update(DragMsg::Release), None);
    }

    #[test]
    fn handle_press_with_unknown_point_rebases_on_first_move() {
        let mut s = S::default();
        s.update(DragMsg::Press(1, "x".into(), p(f32::NAN, f32::NAN)));
        s.update(DragMsg::Move(p(100.0, 100.0)));
        assert!(!s.dragging().unwrap().active);
        s.update(DragMsg::Move(p(120.0, 100.0)));
        assert!(s.dragging().unwrap().active);
    }
}

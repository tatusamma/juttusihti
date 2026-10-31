//! Relationship map: an interactive graph of articles, entities and events.

use iced::mouse;
use iced::widget::canvas::{self, Cache, Frame, Geometry, Path, Stroke, Text, stroke};
use iced::widget::{
    button, canvas as canvas_widget, checkbox, column, container, pick_list, row, rule, scrollable, text, text_input,
};
use iced::{Color, Element, Fill, Font, Length, Point, Rectangle, Renderer, Size, Task as Cmd, Theme, Vector, font};
use juttusihti_core::analysis::SessionSummary;
use juttusihti_core::graph::{EdgeKind, Graph, GraphQuery, NodeRef, layout};
use juttusihti_core::{ArticleId, Entity, Origin, Status};

use crate::db::Db;
use crate::review::{KindChoice, SessionChoice, kind_label};

const MAX_NODES: usize = 150;

/// A graph with its computed layout positions.
type Laid = Box<(Graph, Vec<(f32, f32)>)>;

#[derive(Debug, Clone)]
pub enum Msg {
    SessionsLoaded(Result<Vec<SessionSummary>, String>),
    Session(SessionChoice),
    EntityQuery(String),
    FindEntity,
    EntitiesFound(Result<Vec<Entity>, String>),
    EntityPicked(EntityChoice),
    Hops(u8),
    Suggested(bool),
    Entities(bool),
    Highlight(HighlightChoice),
    Search(String),
    Built(Result<Laid, String>),
    Select(Option<usize>),
    /// Handled by the app.
    OpenArticle(ArticleId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityChoice(pub i64, pub String);

impl std::fmt::Display for EntityChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HighlightChoice(pub Option<KindChoice>);

impl std::fmt::Display for HighlightChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            None => f.write_str("All link types"),
            Some(k) => write!(f, "Highlight: {k}"),
        }
    }
}

pub struct State {
    db: Db,
    sessions: Vec<SessionSummary>,
    session: Option<i64>,
    seeds: Vec<NodeRef>,
    seed_label: String,
    entity_query: String,
    entity_results: Vec<EntityChoice>,
    hops: u8,
    suggested: bool,
    entities: bool,
    highlight: HighlightChoice,
    search: String,
    graph: Option<Graph>,
    positions: Vec<(f32, f32)>,
    selected: Option<usize>,
    cache: Cache,
    error: Option<String>,
}

impl State {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            sessions: Vec::new(),
            session: None,
            seeds: Vec::new(),
            seed_label: String::new(),
            entity_query: String::new(),
            entity_results: Vec::new(),
            hops: 2,
            suggested: true,
            entities: true,
            highlight: HighlightChoice(None),
            search: String::new(),
            graph: None,
            positions: Vec::new(),
            selected: None,
            cache: Cache::new(),
            error: None,
        }
    }

    pub fn load(&self) -> Cmd<Msg> {
        let db = self.db.clone();
        Cmd::perform(async move { db.call(|s| s.recent_sessions(50)).await }, Msg::SessionsLoaded)
    }

    /// Centres the map on one article (from the reader).
    pub fn focus_article(&mut self, id: ArticleId, title: &str) -> Cmd<Msg> {
        self.seeds = vec![NodeRef::Article(id)];
        self.seed_label = format!("Article: {title}");
        self.session = None;
        Cmd::batch([self.load(), self.build()])
    }

    fn build(&mut self) -> Cmd<Msg> {
        if self.seeds.is_empty() {
            return Cmd::none();
        }
        let q = GraphQuery {
            seeds: self.seeds.clone(),
            hops: self.hops,
            include_suggested: self.suggested,
            include_entities: self.entities,
            kinds: Vec::new(),
            max_nodes: MAX_NODES,
        };
        let db = self.db.clone();
        Cmd::perform(
            async move {
                db.call(move |s| -> juttusihti_core::Result<Laid> {
                    let g = s.graph(&q)?;
                    let edges: Vec<(usize, usize)> = g.edges.iter().map(|e| (e.a, e.b)).collect();
                    let pos = layout(g.nodes.len(), &edges, 300);
                    Ok(Box::new((g, pos)))
                })
                .await
            },
            Msg::Built,
        )
    }

    pub fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::SessionsLoaded(Ok(s)) => self.sessions = s,
            Msg::SessionsLoaded(Err(e)) | Msg::EntitiesFound(Err(e)) | Msg::Built(Err(e)) => self.error = Some(e),
            Msg::Session(SessionChoice(id, label)) => {
                self.session = Some(id);
                self.seed_label = label;
                let db = self.db.clone();
                return Cmd::perform(
                    async move {
                        db.call(move |s| -> juttusihti_core::Result<Laid> {
                            let seeds = s.session_graph_seeds(id, 60)?;
                            let g =
                                s.graph(&GraphQuery { seeds, hops: 1, max_nodes: MAX_NODES, ..Default::default() })?;
                            let edges: Vec<(usize, usize)> = g.edges.iter().map(|e| (e.a, e.b)).collect();
                            let pos = layout(g.nodes.len(), &edges, 300);
                            Ok(Box::new((g, pos)))
                        })
                        .await
                    },
                    Msg::Built,
                );
            }
            Msg::EntityQuery(q) => self.entity_query = q,
            Msg::FindEntity => {
                let q = self.entity_query.trim().to_string();
                if q.is_empty() {
                    return Cmd::none();
                }
                let db = self.db.clone();
                return Cmd::perform(
                    async move { db.call(move |s| s.find_entities(&q, 30)).await },
                    Msg::EntitiesFound,
                );
            }
            Msg::EntitiesFound(Ok(list)) => {
                self.entity_results =
                    list.into_iter().map(|e| EntityChoice(e.id, format!("{} ({})", e.name, e.kind.as_str()))).collect();
                if self.entity_results.is_empty() {
                    self.error = Some("No matching entities.".into());
                }
            }
            Msg::EntityPicked(EntityChoice(id, label)) => {
                self.seeds = vec![NodeRef::Entity(id)];
                self.seed_label = format!("Entity: {label}");
                self.session = None;
                return self.build();
            }
            Msg::Hops(h) => {
                self.hops = h;
                return self.rebuild_current();
            }
            Msg::Suggested(on) => {
                self.suggested = on;
                return self.rebuild_current();
            }
            Msg::Entities(on) => {
                self.entities = on;
                return self.rebuild_current();
            }
            Msg::Highlight(h) => {
                self.highlight = h;
                self.cache.clear();
            }
            Msg::Search(s) => {
                self.search = s;
                self.cache.clear();
            }
            Msg::Built(Ok(b)) => {
                let (g, pos) = *b;
                self.graph = Some(g);
                self.positions = pos;
                self.selected = None;
                self.error = None;
                self.cache.clear();
            }
            Msg::Select(i) => {
                self.selected = i;
                self.cache.clear();
            }
            Msg::OpenArticle(_) => {}
        }
        Cmd::none()
    }

    fn rebuild_current(&mut self) -> Cmd<Msg> {
        if self.seeds.is_empty()
            && let Some(id) = self.session
        {
            let label = self.seed_label.clone();
            return self.update(Msg::Session(SessionChoice(id, label)));
        }
        self.build()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let bold = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
        let sessions: Vec<SessionChoice> = self
            .sessions
            .iter()
            .map(|s| SessionChoice(s.id, format!("Session #{} · {}", s.id, s.range_label)))
            .collect();
        let selected_session = sessions.iter().find(|c| Some(c.0) == self.session).cloned();
        let mut highlights = vec![HighlightChoice(None)];
        highlights.extend(KindChoice::ALL.iter().map(|k| HighlightChoice(Some(*k))));
        let controls = column![
            text("Relationship map").size(22).font(bold),
            row![
                pick_list(sessions, selected_session, Msg::Session).placeholder("Links from a session…"),
                text_input("or find an entity", &self.entity_query)
                    .on_input(Msg::EntityQuery)
                    .on_submit(Msg::FindEntity)
                    .width(200),
                pick_list(self.entity_results.clone(), None::<EntityChoice>, Msg::EntityPicked).placeholder("Matches"),
            ]
            .spacing(8)
            .align_y(iced::Center),
            row![
                text("Hops").size(12),
                pick_list(&[1u8, 2, 3][..], Some(self.hops), Msg::Hops),
                checkbox(self.suggested).label("Show AI suggestions").on_toggle(Msg::Suggested),
                checkbox(self.entities).label("Show entities").on_toggle(Msg::Entities),
                pick_list(highlights, Some(self.highlight), Msg::Highlight),
                text_input("Search in map", &self.search).on_input(Msg::Search).width(160),
            ]
            .spacing(10)
            .align_y(iced::Center),
            text(if self.seed_label.is_empty() {
                "Choose a session or entity, or use “Map” on an article.".into()
            } else {
                self.seed_label.clone()
            })
            .size(12),
        ]
        .spacing(8)
        .padding(16);

        let mut layout_col = column![controls].height(Fill);
        if let Some(e) = &self.error {
            layout_col = layout_col.push(container(text(e).size(12)).padding([4, 16]).style(container::danger));
        }
        let Some(graph) = &self.graph else {
            return layout_col.push(container(text("No map yet.")).center(Fill)).into();
        };
        if graph.truncated {
            layout_col = layout_col.push(
                container(
                    text(format!("Showing the first {MAX_NODES} nodes; narrow the map with fewer hops or filters."))
                        .size(12),
                )
                .padding([0, 16]),
            );
        }
        let program = GraphCanvas {
            graph,
            positions: &self.positions,
            selected: self.selected,
            highlight: self.highlight.0.map(|k| k.0),
            search: self.search.trim().to_lowercase(),
            cache: &self.cache,
        };
        let canvas = canvas_widget(program).width(Fill).height(Fill);
        layout_col
            .push(
                row![container(canvas).width(Length::FillPortion(3)), rule::vertical(1), self.view_details(graph)]
                    .height(Fill),
            )
            .into()
    }

    fn view_details<'a>(&'a self, g: &'a Graph) -> Element<'a, Msg> {
        let bold = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
        let mut col =
            column![text(format!("{} nodes, {} links", g.nodes.len(), g.edges.len())).size(12)].spacing(6).padding(12);
        col = col.push(text("Solid: confirmed · dashed: AI suggestion · ● article · ■ entity · ◆ event").size(11));
        if let Some(i) = self.selected
            && let Some(n) = g.nodes.get(i)
        {
            col = col.push(text(&n.label).size(15).font(bold)).push(text(&n.detail).size(12));
            if let Some(t) = n.time {
                col = col.push(text(crate::format::full_date(t)).size(12));
            }
            if let NodeRef::Article(id) = n.id {
                col = col.push(button(text("Open in reader").size(12)).on_press(Msg::OpenArticle(id)));
            }
            col = col.push(text("Links").size(13).font(bold));
            for e in g.edges.iter().filter(|e| e.a == i || e.b == i) {
                let other = if e.a == i { e.b } else { e.a };
                let kind = match e.kind {
                    EdgeKind::Link(k) => kind_label(k),
                    EdgeKind::Mentions => "Mentions",
                    EdgeKind::InEvent => "Event",
                };
                let status = match (e.origin, e.status) {
                    (_, Status::Confirmed) => "confirmed",
                    (Origin::Ai, _) => "AI suggestion",
                    _ => "pending",
                };
                col = col.push(
                    button(text(format!("{kind} ({status}) → {}", g.nodes[other].label)).size(11))
                        .style(button::text)
                        .padding(0)
                        .on_press(Msg::Select(Some(other))),
                );
            }
        } else {
            col = col.push(text("Click a node to see its links. Scroll to zoom, drag to pan.").size(12));
        }
        scrollable(col).width(Length::FillPortion(1)).into()
    }
}

struct GraphCanvas<'a> {
    graph: &'a Graph,
    positions: &'a [(f32, f32)],
    selected: Option<usize>,
    highlight: Option<juttusihti_core::RelationshipKind>,
    search: String,
    cache: &'a Cache,
}

#[derive(Debug)]
pub struct Camera {
    zoom: f32,
    offset: Vector,
    drag: Option<Point>,
}

impl Default for Camera {
    fn default() -> Self {
        Self { zoom: 1.0, offset: Vector::new(0.0, 0.0), drag: None }
    }
}

impl GraphCanvas<'_> {
    fn to_screen(&self, cam: &Camera, size: Size, (x, y): (f32, f32)) -> Point {
        let scale = size.width.min(size.height) * 0.45 * cam.zoom;
        Point::new(size.width / 2.0 + x * scale + cam.offset.x, size.height / 2.0 + y * scale + cam.offset.y)
    }

    fn hit(&self, cam: &Camera, size: Size, p: Point) -> Option<usize> {
        self.positions
            .iter()
            .enumerate()
            .map(|(i, pos)| (i, self.to_screen(cam, size, *pos).distance(p)))
            .filter(|(_, d)| *d <= 10.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }
}

impl canvas::Program<Msg> for GraphCanvas<'_> {
    type State = Camera;

    fn update(
        &self,
        cam: &mut Camera,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Msg>> {
        let pos = cursor.position_in(bounds);
        match event {
            canvas::Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let p = pos?;
                let dy = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => *y,
                    mouse::ScrollDelta::Pixels { y, .. } => *y / 40.0,
                };
                let old = cam.zoom;
                cam.zoom = (cam.zoom * (1.0 + dy * 0.15)).clamp(0.3, 12.0);
                // Zoom around the cursor.
                let c = Vector::new(p.x - bounds.width / 2.0, p.y - bounds.height / 2.0);
                cam.offset = c - (c - cam.offset) * (cam.zoom / old);
                self.cache.clear();
                Some(canvas::Action::request_redraw().and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = pos?;
                if let Some(i) = self.hit(cam, bounds.size(), p) {
                    return Some(canvas::Action::publish(Msg::Select(Some(i))).and_capture());
                }
                cam.drag = Some(p);
                Some(canvas::Action::capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let (Some(start), Some(p)) = (cam.drag, pos) else { return None };
                cam.offset += p - start;
                cam.drag = Some(p);
                self.cache.clear();
                Some(canvas::Action::request_redraw().and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                cam.drag.take().map(|_| canvas::Action::capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        cam: &Camera,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let palette = theme.palette();
        let data = crate::style::data_colors(theme);
        let geometry = self.cache.draw(renderer, bounds.size(), |frame: &mut Frame| {
            let size = bounds.size();
            let faded = |c: Color| Color { a: 0.18, ..c };
            let neighbours: Vec<usize> = match self.selected {
                Some(s) => self
                    .graph
                    .edges
                    .iter()
                    .filter(|e| e.a == s || e.b == s)
                    .map(|e| if e.a == s { e.b } else { e.a })
                    .collect(),
                None => Vec::new(),
            };
            for e in &self.graph.edges {
                let (a, b) =
                    (self.to_screen(cam, size, self.positions[e.a]), self.to_screen(cam, size, self.positions[e.b]));
                let mut color = match e.kind {
                    EdgeKind::Link(juttusihti_core::RelationshipKind::Duplicate) => data.slate,
                    EdgeKind::Link(juttusihti_core::RelationshipKind::ConflictingClaim) => data.clay,
                    EdgeKind::Link(_) => data.amber,
                    EdgeKind::Mentions => Color { a: 0.5, ..palette.text },
                    EdgeKind::InEvent => data.sage,
                };
                let mut width = if matches!(e.kind, EdgeKind::Link(_)) { 2.0 } else { 1.0 };
                if let Some(h) = self.highlight {
                    if e.kind == EdgeKind::Link(h) {
                        width = 3.5;
                    } else {
                        color = faded(color);
                    }
                }
                if self.selected.is_some_and(|s| e.a == s || e.b == s) {
                    width += 1.0;
                }
                let dashed = e.status != Status::Confirmed;
                let stroke = Stroke {
                    width,
                    style: stroke::Style::Solid(color),
                    line_dash: if dashed {
                        canvas::LineDash { segments: &[6.0, 4.0], offset: 0 }
                    } else {
                        canvas::LineDash::default()
                    },
                    ..Stroke::default()
                };
                frame.stroke(&Path::line(a, b), stroke);
            }
            for (i, n) in self.graph.nodes.iter().enumerate() {
                let p = self.to_screen(cam, size, self.positions[i]);
                let matches = !self.search.is_empty() && n.label.to_lowercase().contains(&self.search);
                let is_sel = self.selected == Some(i);
                let color = match n.id {
                    NodeRef::Article(_) => data.amber,
                    NodeRef::Entity(_) => data.sage,
                    NodeRef::Event(_) => data.clay,
                };
                let r = if is_sel || matches { 8.0 } else { 5.5 };
                let shape = match n.id {
                    NodeRef::Article(_) => Path::circle(p, r),
                    NodeRef::Entity(_) => Path::rectangle(Point::new(p.x - r, p.y - r), Size::new(2.0 * r, 2.0 * r)),
                    NodeRef::Event(_) => Path::new(|b| {
                        b.move_to(Point::new(p.x, p.y - r - 2.0));
                        b.line_to(Point::new(p.x + r + 2.0, p.y));
                        b.line_to(Point::new(p.x, p.y + r + 2.0));
                        b.line_to(Point::new(p.x - r - 2.0, p.y));
                        b.close();
                    }),
                };
                frame.fill(&shape, color);
                if is_sel || matches {
                    frame.stroke(&shape, Stroke::default().with_width(2.0).with_color(palette.text));
                }
                let show_label =
                    is_sel || matches || neighbours.contains(&i) || cam.zoom >= 1.8 || self.graph.nodes.len() <= 25;
                if show_label {
                    let mut label: String = n.label.chars().take(42).collect();
                    if n.label.chars().count() > 42 {
                        label.push('…');
                    }
                    frame.fill_text(Text {
                        content: label,
                        position: Point::new(p.x + r + 4.0, p.y - 7.0),
                        color: palette.text,
                        size: 12.0.into(),
                        ..Text::default()
                    });
                }
            }
        });
        vec![geometry]
    }

    fn mouse_interaction(&self, cam: &Camera, bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        match cursor.position_in(bounds) {
            Some(_) if cam.drag.is_some() => mouse::Interaction::Grabbing,
            Some(p) if self.hit(cam, bounds.size(), p).is_some() => mouse::Interaction::Pointer,
            Some(_) => mouse::Interaction::Grab,
            None => mouse::Interaction::default(),
        }
    }
}

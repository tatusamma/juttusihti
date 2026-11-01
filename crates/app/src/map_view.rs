//! Geographic map: OpenStreetMap raster tiles (only when the user enables
//! them) with research locations. Places named by AI have no coordinates
//! until the user places them.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use iced::mouse;
use iced::widget::canvas::{self, Cache, Frame, Geometry, Path, Stroke, Text};
use iced::widget::image::Handle;
use iced::widget::{Space, button, canvas as canvas_widget, column, container, pick_list, row, rule, scrollable, text};
use iced::{Center, Color, Element, Fill, Length, Point, Rectangle, Renderer, Size, Task as Cmd, Theme, Vector};
use juttusihti_core::analysis::SessionSummary;
use juttusihti_core::review::LocationRow;
use juttusihti_core::{ArticleId, LocationRole, Status};
use juttusihti_fetch::tiles::{
    ATTRIBUTION, MAX_ZOOM, OSM_COPYRIGHT_URL, OSM_FIX_THE_MAP_URL, OSM_TILE_URL, TILE_SIZE, TileCache, TileClient,
    TileId, lonlat_to_world, visible_tiles, world_to_lonlat,
};

use crate::db::Db;
use crate::icons::Icon;
use crate::review::SessionChoice;
use crate::style::{self, SEMIBOLD, SIZE_BODY, SIZE_SMALL, SPACE};

pub const KEY_TILES_ENABLED: &str = "map_tiles_enabled";
pub const KEY_TILE_URL: &str = "tile_url";
/// Decoded tiles kept in memory; the disk cache holds the rest.
const MEMORY_TILES: usize = 400;

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Result<(Vec<SessionSummary>, bool, String), String>),
    LocationsLoaded(Result<Vec<LocationRow>, String>),
    Session(SessionChoice),
    EnableTiles(bool),
    Viewport(Size),
    Pan(Vector),
    PanEnd,
    Zoom(i8, Point),
    TileLoaded(TileId, Result<Vec<u8>, String>),
    Select(i64),
    PlaceMode(i64),
    MapClicked(f64, f64),
    Decide(i64, Status),
    ClearCoordinates(i64),
    CenterOn(i64),
    Done(Result<(), String>),
    OpenUrl(&'static str),
    /// Handled by the app.
    OpenArticle(ArticleId),
}

pub struct State {
    db: Db,
    tile_dir: PathBuf,
    client: Option<TileClient>,
    tiles_enabled: bool,
    template: String,
    center: (f64, f64),
    zoom: u8,
    size: Size,
    tiles: HashMap<TileId, Handle>,
    pending: HashSet<TileId>,
    failed: usize,
    sessions: Vec<SessionSummary>,
    session: Option<i64>,
    locations: Vec<LocationRow>,
    selected: Option<i64>,
    placing: Option<i64>,
    cache: Cache,
    error: Option<String>,
}

impl State {
    pub fn new(db: Db, tile_dir: PathBuf) -> Self {
        Self {
            db,
            tile_dir,
            client: None,
            tiles_enabled: false,
            template: OSM_TILE_URL.into(),
            // Northern Europe.
            center: lonlat_to_world(18.0, 58.0),
            zoom: 4,
            size: Size::new(800.0, 600.0),
            tiles: HashMap::new(),
            pending: HashSet::new(),
            failed: 0,
            sessions: Vec::new(),
            session: None,
            locations: Vec::new(),
            selected: None,
            placing: None,
            cache: Cache::new(),
            error: None,
        }
    }

    pub fn load(&self) -> Cmd<Msg> {
        let db = self.db.clone();
        let settings = Cmd::perform(
            async move {
                db.call(|s| -> juttusihti_core::Result<_> {
                    let enabled = s.setting(KEY_TILES_ENABLED)?.is_some_and(|v| v == "1");
                    let template = s.setting(KEY_TILE_URL)?.unwrap_or_else(|| OSM_TILE_URL.into());
                    Ok((s.recent_sessions(50)?, enabled, template))
                })
                .await
            },
            Msg::Loaded,
        );
        Cmd::batch([settings, self.load_locations()])
    }

    fn load_locations(&self) -> Cmd<Msg> {
        let (db, session) = (self.db.clone(), self.session);
        Cmd::perform(async move { db.call(move |s| s.locations(session, 2_000)).await }, Msg::LocationsLoaded)
    }

    fn act(
        &self,
        f: impl FnOnce(&mut juttusihti_core::store::Store) -> juttusihti_core::Result<()> + Send + 'static,
    ) -> Cmd<Msg> {
        let db = self.db.clone();
        Cmd::perform(async move { db.call(f).await }, Msg::Done)
    }

    fn world_px(&self) -> f64 {
        f64::from(1u32 << self.zoom) * f64::from(TILE_SIZE)
    }

    /// Requests the tiles in the current viewport that are not loaded yet.
    fn request_tiles(&mut self) -> Cmd<Msg> {
        if !self.tiles_enabled {
            return Cmd::none();
        }
        if self.client.is_none() {
            match TileClient::new(&self.template, TileCache::new(self.tile_dir.clone())) {
                Ok(c) => self.client = Some(c),
                Err(e) => {
                    self.error = Some(e.to_string());
                    return Cmd::none();
                }
            }
        }
        let client = self.client.clone().expect("set above");
        let visible: Vec<TileId> =
            visible_tiles(self.center, self.zoom, self.size.width, self.size.height).into_iter().map(|t| t.0).collect();
        if self.tiles.len() > MEMORY_TILES {
            let keep: HashSet<TileId> = visible.iter().copied().collect();
            self.tiles.retain(|id, _| keep.contains(id));
        }
        let mut cmds = Vec::new();
        for id in visible {
            if self.tiles.contains_key(&id) || !self.pending.insert(id) {
                continue;
            }
            let c = client.clone();
            cmds.push(Cmd::perform(async move { c.tile(id).await.map_err(|e| e.to_string()) }, move |r| {
                Msg::TileLoaded(id, r)
            }));
        }
        Cmd::batch(cmds)
    }

    fn center_on(&mut self, lat: f64, lon: f64) -> Cmd<Msg> {
        self.center = lonlat_to_world(lon, lat);
        self.zoom = self.zoom.max(9);
        self.cache.clear();
        self.request_tiles()
    }

    pub fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Loaded(Ok((sessions, enabled, template))) => {
                self.sessions = sessions;
                self.tiles_enabled = enabled;
                if template != self.template {
                    self.template = template;
                    self.client = None;
                    self.tiles.clear();
                }
                self.cache.clear();
                return self.request_tiles();
            }
            Msg::Loaded(Err(e)) | Msg::LocationsLoaded(Err(e)) | Msg::Done(Err(e)) => self.error = Some(e),
            Msg::LocationsLoaded(Ok(l)) => {
                self.locations = l;
                self.cache.clear();
            }
            Msg::Session(SessionChoice(id, _)) => {
                self.session = if id == 0 { None } else { Some(id) };
                return self.load_locations();
            }
            Msg::EnableTiles(on) => {
                self.tiles_enabled = on;
                self.cache.clear();
                let db = self.db.clone();
                let save = Cmd::perform(
                    async move { db.call(move |s| s.set_setting(KEY_TILES_ENABLED, if on { "1" } else { "0" })).await },
                    Msg::Done,
                );
                return Cmd::batch([save, self.request_tiles()]);
            }
            Msg::Viewport(size) => {
                self.size = size;
                self.cache.clear();
                return self.request_tiles();
            }
            Msg::Pan(d) => {
                let px = self.world_px();
                self.center.0 = (self.center.0 - f64::from(d.x) / px).rem_euclid(1.0);
                self.center.1 = (self.center.1 - f64::from(d.y) / px).clamp(0.0, 1.0);
                self.cache.clear();
            }
            Msg::PanEnd => return self.request_tiles(),
            Msg::Zoom(dir, cursor) => {
                let new = (i16::from(self.zoom) + i16::from(dir)).clamp(1, i16::from(MAX_ZOOM)) as u8;
                if new == self.zoom {
                    return Cmd::none();
                }
                // Keep the world point under the cursor fixed.
                let off = (f64::from(cursor.x - self.size.width / 2.0), f64::from(cursor.y - self.size.height / 2.0));
                let px = self.world_px();
                let under = (self.center.0 + off.0 / px, self.center.1 + off.1 / px);
                self.zoom = new;
                let px = self.world_px();
                self.center = ((under.0 - off.0 / px).rem_euclid(1.0), (under.1 - off.1 / px).clamp(0.0, 1.0));
                self.cache.clear();
                return self.request_tiles();
            }
            Msg::TileLoaded(id, r) => {
                self.pending.remove(&id);
                match r {
                    Ok(bytes) => {
                        self.tiles.insert(id, Handle::from_bytes(bytes));
                        self.cache.clear();
                    }
                    Err(e) => {
                        self.failed += 1;
                        tracing::debug!(?id, error = %e, "tile failed");
                        if self.failed == 1 || self.failed.is_multiple_of(50) {
                            self.error = Some(format!("Some map tiles could not be loaded: {e}"));
                        }
                    }
                }
            }
            Msg::Select(id) => {
                self.selected = Some(id);
                self.cache.clear();
            }
            Msg::PlaceMode(id) => {
                self.placing = Some(id);
                self.selected = Some(id);
            }
            Msg::MapClicked(lat, lon) => {
                if let Some(id) = self.placing.take() {
                    return self.act(move |s| s.set_location_coordinates(id, Some((lat, lon))));
                }
            }
            Msg::Decide(id, st) => return self.act(move |s| s.decide_location(id, st)),
            Msg::ClearCoordinates(id) => return self.act(move |s| s.set_location_coordinates(id, None)),
            Msg::CenterOn(id) => {
                if let Some((lat, lon)) = self.locations.iter().find(|l| l.id == id).and_then(|l| l.coordinates) {
                    self.selected = Some(id);
                    return self.center_on(lat, lon);
                }
            }
            Msg::Done(Ok(())) => return self.load_locations(),
            Msg::OpenUrl(url) => {
                let _ = open::that_detached(url);
            }
            Msg::OpenArticle(_) => {}
        }
        Cmd::none()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let mut sessions = vec![SessionChoice(0, "All locations".into())];
        sessions.extend(
            self.sessions.iter().map(|s| SessionChoice(s.id, format!("Session #{} · {}", s.id, s.range_label))),
        );
        let selected_session = sessions.iter().find(|c| c.0 == self.session.unwrap_or(0)).cloned();
        let host = self.template.split('/').nth(2).unwrap_or("the tile server").to_string();
        let tiles_note = if self.tiles_enabled {
            style::intro(
                format!("Tiles for the area you view are requested from {host}."),
                format!(
                    "Tiles for the area you view are requested from {host}, which sees your IP address and viewing \
                     area. Tiles are cached locally; nothing is downloaded in the background."
                ),
            )
        } else {
            style::note("Map tiles are off: only a coordinate grid is shown and nothing is sent anywhere.").into()
        };
        let mut top = column![
            row![
                style::title("Map"),
                pick_list(sessions, selected_session, Msg::Session)
                    .style(style::pick_list)
                    .menu_style(style::menu)
                    .text_size(SIZE_SMALL),
                style::check(self.tiles_enabled, format!("Load map tiles from {host}")).on_toggle(Msg::EnableTiles),
            ]
            .spacing(SPACE[2])
            .align_y(Center),
            tiles_note,
        ]
        .spacing(SPACE[1])
        .padding([SPACE[2], SPACE[3]]);
        if let Some(id) = self.placing {
            let name = self.locations.iter().find(|l| l.id == id).map_or("", |l| l.place_name.as_str());
            top = top.push(
                container(text(format!("Click the map where “{name}” is.")).size(SIZE_SMALL).style(style::body))
                    .padding([SPACE[0], SPACE[1]])
                    .style(style::card),
            );
        }
        if let Some(e) = &self.error {
            top = top.push(text(e).size(SIZE_SMALL).style(style::danger));
        }

        let program = MapCanvas { state: self };
        let canvas = canvas_widget(program).width(Fill).height(Fill);
        // Tiles overhang the canvas edges, and the CPU renderer clips images only to the enclosing
        // layer. A clipping stack draws every child after the first in a layer of its own bounded
        // by the map, so the tiles sit above an empty base; otherwise they paint over the rail,
        // the header and the places list.
        let mut map = if self.tiles_enabled {
            let markers = canvas_widget(MapMarkers { state: self }).width(Fill).height(Fill);
            let base = Space::new().width(Fill).height(Fill);
            column![iced::widget::stack![base, canvas, markers].clip(true)]
        } else {
            column![canvas]
        };
        if self.tiles_enabled {
            let link = |label: &'static str, url: &'static str| {
                button(text(label).size(SIZE_SMALL).style(style::muted))
                    .style(style::row)
                    .padding([2.0, SPACE[1]])
                    .on_press(Msg::OpenUrl(url))
            };
            map = map.push(
                row![link(ATTRIBUTION, OSM_COPYRIGHT_URL), link("Report a map issue", OSM_FIX_THE_MAP_URL)]
                    .spacing(SPACE[1]),
            );
        }
        column![
            top,
            row![container(map).width(Length::FillPortion(3)), rule::vertical(1), self.view_places()].height(Fill)
        ]
        .into()
    }

    fn view_places(&self) -> Element<'_, Msg> {
        let placed = self.locations.iter().filter(|l| l.coordinates.is_some()).count();
        let mut col = column![
            style::note(format!("{} places, {placed} on the map", self.locations.len())),
            style::note("AI-suggested places have no coordinates. Use “Place on map” after checking the evidence."),
        ]
        .spacing(SPACE[1])
        .padding(SPACE[2]);
        for l in &self.locations {
            let selected = self.selected == Some(l.id);
            let status = match l.status {
                Status::Confirmed => "confirmed",
                Status::Suggested => "AI suggestion",
                Status::Rejected => "rejected",
            };
            let mut item = column![
                button(
                    text(format!("{}{}", if l.ambiguous { "⚠ " } else { "" }, l.place_name))
                        .size(SIZE_BODY)
                        .font(SEMIBOLD)
                )
                .style(style::row)
                .padding(0)
                .on_press(Msg::Select(l.id)),
                style::note(format!("{} · {status}", role_label(l.role))),
                style::note(match l.coordinates {
                    Some((lat, lon)) =>
                        format!("{lat:.4}, {lon:.4} ({})", l.coordinate_source.as_deref().unwrap_or("?")),
                    None => "Not placed".into(),
                }),
            ]
            .spacing(2);
            if selected {
                if let Some(e) = &l.evidence {
                    item = item.push(text(format!("“{e}”")).size(SIZE_SMALL).style(style::faint));
                }
                item = item.push(
                    button(text(l.article_title.clone()).size(SIZE_SMALL).style(style::muted))
                        .style(style::row)
                        .padding(0)
                        .on_press(Msg::OpenArticle(l.article_id)),
                );
                let mut actions = row![].spacing(SPACE[0]);
                if l.status == Status::Suggested {
                    actions = actions.push(style::quiet_button(
                        Some(Icon::Check),
                        "Confirm",
                        style::quiet,
                        Some(Msg::Decide(l.id, Status::Confirmed)),
                    ));
                }
                actions = actions
                    .push(style::quiet_button(None, "Reject", style::quiet, Some(Msg::Decide(l.id, Status::Rejected))))
                    .push(style::quiet_button(None, "Place on map", style::quiet, Some(Msg::PlaceMode(l.id))));
                if l.coordinates.is_some() {
                    actions = actions
                        .push(style::quiet_button(None, "Go to", style::quiet, Some(Msg::CenterOn(l.id))))
                        .push(style::quiet_button(None, "Clear", style::quiet, Some(Msg::ClearCoordinates(l.id))));
                }
                item = item.push(actions);
            }
            let card = container(item).padding(SPACE[1]).width(Fill);
            col = col.push(if selected { card.style(style::card) } else { card });
        }
        scrollable(col).width(Length::FillPortion(1)).into()
    }
}

fn role_label(r: LocationRole) -> &'static str {
    match r {
        LocationRole::Event => "event location",
        LocationRole::Publisher => "publisher location",
        LocationRole::EntityAssociated => "associated with an entity",
        LocationRole::Mentioned => "mentioned",
    }
}

struct MapCanvas<'a> {
    state: &'a State,
}

#[derive(Default)]
pub struct Interaction {
    drag: Option<Point>,
    last_size: Option<Size>,
}

impl MapCanvas<'_> {
    fn left_top(&self, size: Size) -> (f64, f64) {
        let px = self.state.world_px();
        (
            self.state.center.0 * px - f64::from(size.width) / 2.0,
            self.state.center.1 * px - f64::from(size.height) / 2.0,
        )
    }

    /// Screen position of a coordinate, choosing the world copy nearest the centre.
    fn to_screen(&self, size: Size, lat: f64, lon: f64) -> Point {
        let px = self.state.world_px();
        let (wx, wy) = lonlat_to_world(lon, lat);
        let mut dx = wx - self.state.center.0;
        if dx > 0.5 {
            dx -= 1.0;
        } else if dx < -0.5 {
            dx += 1.0;
        }
        Point::new(size.width / 2.0 + (dx * px) as f32, size.height / 2.0 + ((wy - self.state.center.1) * px) as f32)
    }

    fn to_lonlat(&self, size: Size, p: Point) -> (f64, f64) {
        let px = self.state.world_px();
        let (left, top) = self.left_top(size);
        world_to_lonlat((left + f64::from(p.x)) / px, (top + f64::from(p.y)) / px)
    }

    fn hit(&self, size: Size, p: Point) -> Option<i64> {
        self.state
            .locations
            .iter()
            .filter_map(|l| l.coordinates.map(|(la, lo)| (l.id, self.to_screen(size, la, lo).distance(p))))
            .filter(|(_, d)| *d <= 9.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }
}

impl MapCanvas<'_> {
    /// One marker: a solid dot with a white edge when confirmed, a ring when
    /// only suggested, plus its name when selected or zoomed in.
    fn draw_marker(
        &self,
        frame: &mut Frame,
        size: Size,
        l: &LocationRow,
        data: &crate::style::DataColors,
        label_color: Color,
    ) {
        let Some((lat, lon)) = l.coordinates else { return };
        let p = self.to_screen(size, lat, lon);
        let color = match l.role {
            LocationRole::Event => data.clay,
            LocationRole::Publisher => data.amber,
            LocationRole::EntityAssociated => data.sage,
            LocationRole::Mentioned => data.slate,
        };
        let selected = self.state.selected == Some(l.id);
        let dot = Path::circle(p, if selected { 8.0 } else { 6.0 });
        if l.status == Status::Confirmed {
            frame.fill(&dot, color);
            frame.stroke(&dot, Stroke::default().with_width(1.5).with_color(Color::WHITE));
        } else {
            frame.fill(&dot, Color { a: 0.35, ..color });
            frame.stroke(&dot, Stroke::default().with_width(2.0).with_color(color));
        }
        if selected || self.state.zoom >= 7 {
            frame.fill_text(Text {
                content: l.place_name.clone(),
                position: Point::new(p.x + 9.0, p.y - 7.0),
                color: label_color,
                size: SIZE_SMALL.into(),
                ..Text::default()
            });
        }
    }
}

/// Markers, labels and the attribution drawn in a layer above the map tiles: a canvas draws images above
/// its own shapes, so with tiles on the dots must live in a second canvas.
struct MapMarkers<'a> {
    state: &'a State,
}

impl canvas::Program<Msg> for MapMarkers<'_> {
    type State = ();

    fn draw(
        &self,
        _st: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _c: mouse::Cursor,
    ) -> Vec<Geometry> {
        let data = crate::style::data_colors(theme);
        let map = MapCanvas { state: self.state };
        let size = bounds.size();
        let mut frame = Frame::new(renderer, size);
        for l in &self.state.locations {
            map.draw_marker(&mut frame, size, l, &data, Color::from_rgb(0.1, 0.1, 0.1));
        }
        // Attribution on the map itself, bottom-right.
        let w = 214.0;
        frame.fill_rectangle(
            Point::new(size.width - w, size.height - 20.0),
            Size::new(w, 20.0),
            Color::from_rgba(1.0, 1.0, 1.0, 0.8),
        );
        frame.fill_text(Text {
            content: ATTRIBUTION.into(),
            position: Point::new(size.width - w + 6.0, size.height - 18.0),
            color: Color::from_rgb(0.1, 0.1, 0.1),
            size: SIZE_SMALL.into(),
            ..Text::default()
        });
        vec![frame.into_geometry()]
    }
}

impl canvas::Program<Msg> for MapCanvas<'_> {
    type State = Interaction;

    fn update(
        &self,
        st: &mut Interaction,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Msg>> {
        if st.last_size != Some(bounds.size()) {
            st.last_size = Some(bounds.size());
            return Some(canvas::Action::publish(Msg::Viewport(bounds.size())));
        }
        let pos = cursor.position_in(bounds);
        match event {
            canvas::Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let p = pos?;
                let y = match delta {
                    mouse::ScrollDelta::Lines { y, .. } | mouse::ScrollDelta::Pixels { y, .. } => *y,
                };
                let dir = if y > 0.0 {
                    1
                } else if y < 0.0 {
                    -1
                } else {
                    return None;
                };
                Some(canvas::Action::publish(Msg::Zoom(dir, p)).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = pos?;
                if self.state.placing.is_some() {
                    let (lon, lat) = self.to_lonlat(bounds.size(), p);
                    return Some(canvas::Action::publish(Msg::MapClicked(lat, lon)).and_capture());
                }
                if let Some(id) = self.hit(bounds.size(), p) {
                    return Some(canvas::Action::publish(Msg::Select(id)).and_capture());
                }
                st.drag = Some(p);
                Some(canvas::Action::capture())
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let (Some(start), Some(p)) = (st.drag, pos) else { return None };
                st.drag = Some(p);
                Some(canvas::Action::publish(Msg::Pan(p - start)).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                st.drag.take().map(|_| canvas::Action::publish(Msg::PanEnd).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _st: &Interaction,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _c: mouse::Cursor,
    ) -> Vec<Geometry> {
        let pal = theme.palette();
        let data = crate::style::data_colors(theme);
        let s = self.state;
        let geometry = s.cache.draw(renderer, bounds.size(), |frame: &mut Frame| {
            let size = bounds.size();
            frame.fill_rectangle(Point::ORIGIN, size, Color { a: 1.0, ..pal.background });
            if s.tiles_enabled {
                for (id, x, y) in visible_tiles(s.center, s.zoom, size.width, size.height) {
                    if let Some(h) = s.tiles.get(&id) {
                        frame.draw_image(Rectangle::new(Point::new(x, y), Size::new(TILE_SIZE, TILE_SIZE)), h);
                    }
                }
            } else {
                // Reference grid every 10° (30° when zoomed far out).
                let step = if s.zoom <= 3 { 30.0 } else { 10.0 };
                let grid = Stroke::default().with_color(Color { a: 0.15, ..pal.text });
                let mut lon = -180.0;
                while lon <= 180.0 {
                    let a = self.to_screen(size, 85.0, lon);
                    let b = self.to_screen(size, -85.0, lon);
                    frame.stroke(&Path::line(a, b), grid);
                    lon += step;
                }
                let mut lat = -80.0;
                while lat <= 80.0 {
                    let a = self.to_screen(size, lat, -179.9);
                    let b = self.to_screen(size, lat, 179.9);
                    frame.stroke(&Path::line(Point::new(0.0, a.y), Point::new(size.width, b.y)), grid);
                    frame.fill_text(Text {
                        content: format!("{lat}°"),
                        position: Point::new(4.0, a.y - 14.0),
                        color: Color { a: 0.5, ..pal.text },
                        size: SIZE_SMALL.into(),
                        ..Text::default()
                    });
                    lat += step;
                }
            }
            if !s.tiles_enabled {
                for l in &s.locations {
                    self.draw_marker(frame, size, l, &data, pal.text);
                }
            }
        });
        vec![geometry]
    }

    fn mouse_interaction(&self, st: &Interaction, bounds: Rectangle, cursor: mouse::Cursor) -> mouse::Interaction {
        match cursor.position_in(bounds) {
            Some(_) if self.state.placing.is_some() => mouse::Interaction::Crosshair,
            Some(_) if st.drag.is_some() => mouse::Interaction::Grabbing,
            Some(p) if self.hit(bounds.size(), p).is_some() => mouse::Interaction::Pointer,
            Some(_) => mouse::Interaction::Grab,
            None => mouse::Interaction::default(),
        }
    }
}

//! Timeline: coverage over time per source, story threads and events.

use std::collections::{BTreeMap, HashMap};

use iced::mouse;
use iced::widget::canvas::{self, Cache, Frame, Geometry, Path, Stroke, Text};
use iced::widget::{canvas as canvas_widget, column, container, pick_list, row, scrollable, text, text_input};
use iced::{Center, Color, Element, Fill, Point, Rectangle, Renderer, Size, Task as Cmd, Theme};
use juttusihti_core::search::SearchQuery;
use juttusihti_core::timeline::Timeline;
use juttusihti_core::{ArticleId, Feed, Label};

use crate::db::Db;
use crate::format;
use crate::icons::Icon;
use crate::style::{self, SIZE_BODY, SIZE_SMALL, SPACE};

const LIMIT: u32 = 2_000;
const DAY: i64 = 86_400;
const LEFT: f32 = 170.0;
const TOP: f32 = 46.0;
const BOTTOM: f32 = 70.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice(pub Option<i64>, pub String);

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    ListsLoaded(Result<(Vec<Feed>, Vec<Label>), String>),
    Days(i64),
    Shift(i64),
    Today,
    Query(String),
    Submit,
    Label(Choice),
    Source(Choice),
    OnlyThreads(bool),
    Loaded(Result<Box<Timeline>, String>),
    Hover(Option<usize>),
    /// Handled by the app.
    OpenArticle(ArticleId),
}

pub struct State {
    db: Db,
    feeds: Vec<Feed>,
    labels: Vec<Label>,
    days: i64,
    end: i64,
    query: String,
    label: Option<i64>,
    source: Option<i64>,
    only_threads: bool,
    data: Option<Timeline>,
    hover: Option<usize>,
    cache: Cache,
    error: Option<String>,
}

impl State {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            feeds: Vec::new(),
            labels: Vec::new(),
            days: 30,
            end: next_midnight(),
            query: String::new(),
            label: None,
            source: None,
            only_threads: false,
            data: None,
            hover: None,
            cache: Cache::new(),
            error: None,
        }
    }

    fn range(&self) -> (i64, i64) {
        (self.end - self.days * DAY, self.end)
    }

    pub fn load(&mut self) -> Cmd<Msg> {
        let db = self.db.clone();
        let lists = Cmd::perform(
            async move { db.call(|s| -> juttusihti_core::Result<_> { Ok((s.feeds()?, s.labels()?)) }).await },
            Msg::ListsLoaded,
        );
        Cmd::batch([lists, self.fetch()])
    }

    fn fetch(&mut self) -> Cmd<Msg> {
        let (from, to) = self.range();
        let q = SearchQuery {
            text: self.query.trim().to_string(),
            from: Some(from),
            to: Some(to),
            feed_ids: self.source.into_iter().collect(),
            label_ids: self.label.into_iter().collect(),
            ..Default::default()
        };
        let db = self.db.clone();
        Cmd::perform(async move { db.call(move |s| s.timeline(&q, LIMIT).map(Box::new)).await }, Msg::Loaded)
    }

    pub fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::ListsLoaded(Ok((f, l))) => {
                self.feeds = f;
                self.labels = l;
            }
            Msg::ListsLoaded(Err(e)) | Msg::Loaded(Err(e)) => self.error = Some(e),
            Msg::Days(d) => {
                self.days = d;
                return self.fetch();
            }
            Msg::Shift(dir) => {
                self.end += dir * (self.days * DAY / 2).max(DAY);
                return self.fetch();
            }
            Msg::Today => {
                self.end = next_midnight();
                return self.fetch();
            }
            Msg::Query(q) => self.query = q,
            Msg::Submit => return self.fetch(),
            Msg::Label(Choice(id, _)) => {
                self.label = id;
                return self.fetch();
            }
            Msg::Source(Choice(id, _)) => {
                self.source = id;
                return self.fetch();
            }
            Msg::OnlyThreads(on) => {
                self.only_threads = on;
                self.cache.clear();
            }
            Msg::Loaded(Ok(t)) => {
                self.data = Some(*t);
                self.hover = None;
                self.error = None;
                self.cache.clear();
            }
            Msg::Hover(h) => {
                if self.hover != h {
                    self.hover = h;
                    self.cache.clear();
                }
            }
            Msg::OpenArticle(_) => {}
        }
        Cmd::none()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let mut labels = vec![Choice(None, "All labels".into())];
        labels.extend(self.labels.iter().map(|l| Choice(Some(l.id), l.name.clone())));
        let mut sources = vec![Choice(None, "All sources".into())];
        sources.extend(self.feeds.iter().map(|f| Choice(Some(f.id), f.title.clone())));
        let sel_label = labels.iter().find(|c| c.0 == self.label).cloned();
        let sel_source = sources.iter().find(|c| c.0 == self.source).cloned();
        let (from, to) = self.range();
        let zooms = [(7, "Week"), (30, "Month"), (90, "Quarter"), (365, "Year")];
        let controls = column![
            row![
                style::title("Timeline"),
                style::note(format!("{} – {}", format::full_date(from), format::full_date(to - 1))),
            ]
            .spacing(SPACE[2])
            .align_y(Center),
            row![
                style::icon_button(Icon::ChevronLeft, "Earlier", SIZE_BODY, Some(Msg::Shift(-1))),
                style::segmented(zooms, self.days, Msg::Days),
                style::icon_button(Icon::ChevronRight, "Later", SIZE_BODY, Some(Msg::Shift(1))),
                style::quiet_button(None, "Today", style::quiet, Some(Msg::Today)),
                pick_list(labels, sel_label, Msg::Label)
                    .style(style::pick_list)
                    .menu_style(style::menu)
                    .text_size(SIZE_SMALL),
                pick_list(sources, sel_source, Msg::Source)
                    .style(style::pick_list)
                    .menu_style(style::menu)
                    .text_size(SIZE_SMALL),
                text_input("Filter by search query", &self.query)
                    .style(style::text_input)
                    .on_input(Msg::Query)
                    .on_submit(Msg::Submit)
                    .size(SIZE_SMALL)
                    .padding(SPACE[1])
                    .width(200),
                style::check(self.only_threads, "Only story threads").on_toggle(Msg::OnlyThreads),
            ]
            .spacing(SPACE[1])
            .align_y(Center)
            .wrap()
            .vertical_spacing(SPACE[1]),
            style::note(
                "● published date   ○ no date in feed (fetch time)   lines join linked stories; the ringed dot \
                 is the first mention   ◆ events with a known date"
            ),
        ]
        .spacing(SPACE[2])
        .padding(SPACE[3]);
        let mut col = column![controls].height(Fill);
        if let Some(e) = &self.error {
            col = col.push(container(text(e).size(SIZE_SMALL).style(style::danger)).padding([SPACE[0], SPACE[3]]));
        }
        let Some(t) = &self.data else { return col.push(container(style::note("Loading…")).center(Fill)).into() };
        if t.truncated {
            col = col.push(
                container(style::note(format!("Showing the newest {LIMIT} articles; zoom in for more detail.")))
                    .padding([0.0, SPACE[3]]),
            );
        }
        let program =
            TimelineCanvas { t, from, to, only_threads: self.only_threads, hover: self.hover, cache: &self.cache };
        col = col.push(canvas_widget(program).width(Fill).height(Fill));
        if !t.undated_events.is_empty() {
            let mut list =
                column![style::heading("Events without a date in this range (not placed on the axis)")].spacing(2);
            for e in &t.undated_events {
                let when = e.starts_at.map(|s| format!(" — dated {}", format::full_date(s))).unwrap_or_default();
                list = list.push(text(format!("◇ {}{when}", e.title)).size(SIZE_SMALL).style(style::body));
            }
            col = col.push(container(scrollable(list).height(90)).padding([SPACE[0], SPACE[3]]));
        }
        col.into()
    }
}

fn next_midnight() -> i64 {
    let today = chrono::Local::now().date_naive() + chrono::Duration::days(1);
    today
        .and_hms_opt(0, 0, 0)
        .and_then(|n| n.and_local_timezone(chrono::Local).earliest())
        .map(|d| d.timestamp())
        .unwrap_or_else(|| chrono::Utc::now().timestamp())
}

struct TimelineCanvas<'a> {
    t: &'a Timeline,
    from: i64,
    to: i64,
    only_threads: bool,
    hover: Option<usize>,
    cache: &'a Cache,
}

#[derive(Default)]
pub struct Pointer {
    last: Option<usize>,
}

impl TimelineCanvas<'_> {
    fn lanes(&self) -> Vec<(i64, String)> {
        let mut lanes: BTreeMap<String, i64> = BTreeMap::new();
        for a in &self.t.articles {
            lanes.entry(a.feed_title.clone()).or_insert(a.feed_id);
        }
        lanes.into_iter().map(|(t, id)| (id, t)).collect()
    }

    fn x(&self, size: Size, at: i64) -> f32 {
        let w = (size.width - LEFT - 20.0).max(10.0);
        LEFT + (at - self.from) as f32 / (self.to - self.from).max(1) as f32 * w
    }

    fn points(&self, size: Size) -> Vec<Option<Point>> {
        let lanes = self.lanes();
        let lane_of: HashMap<i64, usize> = lanes.iter().enumerate().map(|(i, (id, _))| (*id, i)).collect();
        let h = (size.height - TOP - BOTTOM).max(10.0);
        let step = h / lanes.len().max(1) as f32;
        self.t
            .articles
            .iter()
            .map(|a| {
                if self.only_threads && a.cluster.is_none() {
                    return None;
                }
                let lane = lane_of.get(&a.feed_id).copied().unwrap_or(0);
                // Small deterministic jitter so same-time articles stay visible.
                let jitter = ((a.id % 7) as f32 - 3.0) * step.min(30.0) * 0.08;
                Some(Point::new(self.x(size, a.at), TOP + step * (lane as f32 + 0.5) + jitter))
            })
            .collect()
    }
}

fn tick_step(days: i64) -> (i64, &'static str) {
    match days {
        0..=10 => (DAY, "%-d.%-m."),
        11..=62 => (7 * DAY, "%-d.%-m."),
        _ => (30 * DAY, "%-m/%Y"),
    }
}

impl canvas::Program<Msg> for TimelineCanvas<'_> {
    type State = Pointer;

    fn update(
        &self,
        st: &mut Pointer,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Msg>> {
        let p = cursor.position_in(bounds)?;
        let nearest = || {
            self.points(bounds.size())
                .iter()
                .enumerate()
                .filter_map(|(i, q)| q.map(|q| (i, q.distance(p))))
                .filter(|(_, d)| *d <= 7.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        };
        match event {
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let h = nearest();
                if h != st.last {
                    st.last = h;
                    return Some(canvas::Action::publish(Msg::Hover(h)));
                }
                None
            }
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                nearest().map(|i| canvas::Action::publish(Msg::OpenArticle(self.t.articles[i].id)).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _st: &Pointer,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _c: mouse::Cursor,
    ) -> Vec<Geometry> {
        let pal = theme.palette();
        let data = crate::style::data_colors(theme);
        let geometry = self.cache.draw(renderer, bounds.size(), |frame: &mut Frame| {
            let size = bounds.size();
            let muted = Color { a: 0.35, ..pal.text };
            let lanes = self.lanes();
            let h = (size.height - TOP - BOTTOM).max(10.0);
            let step = h / lanes.len().max(1) as f32;
            for (i, (_, title)) in lanes.iter().enumerate() {
                let y = TOP + step * (i as f32 + 0.5);
                frame.stroke(
                    &Path::line(Point::new(LEFT, y), Point::new(size.width - 20.0, y)),
                    Stroke::default().with_color(Color { a: 0.12, ..pal.text }),
                );
                frame.fill_text(Text {
                    content: title.chars().take(24).collect(),
                    position: Point::new(8.0, y - 7.0),
                    color: pal.text,
                    size: SIZE_SMALL.into(),
                    ..Text::default()
                });
            }
            // Axis and ticks.
            let axis_y = size.height - BOTTOM + 6.0;
            frame.stroke(
                &Path::line(Point::new(LEFT, axis_y), Point::new(size.width - 20.0, axis_y)),
                Stroke::default().with_color(muted),
            );
            let days = (self.to - self.from) / DAY;
            let (tick, fmt) = tick_step(days);
            let mut t = self.from;
            let mut counts: Vec<(i64, usize)> = Vec::new();
            while t < self.to {
                let x = self.x(size, t);
                frame.stroke(
                    &Path::line(Point::new(x, axis_y), Point::new(x, axis_y + 5.0)),
                    Stroke::default().with_color(muted),
                );
                frame.fill_text(Text {
                    content: format::local(t).map(|d| d.format(fmt).to_string()).unwrap_or_default(),
                    position: Point::new(x + 2.0, axis_y + 6.0),
                    color: muted,
                    size: SIZE_SMALL.into(),
                    ..Text::default()
                });
                let n = self.t.articles.iter().filter(|a| a.at >= t && a.at < t + tick).count();
                counts.push((t, n));
                t += tick;
            }
            // Volume histogram under the axis.
            let max = counts.iter().map(|c| c.1).max().unwrap_or(0).max(1);
            for (t, n) in &counts {
                let (x0, x1) = (self.x(size, *t), self.x(size, (*t + tick).min(self.to)));
                let bh = *n as f32 / max as f32 * (BOTTOM - 30.0);
                if bh > 0.0 {
                    frame.fill_rectangle(
                        Point::new(x0 + 1.0, size.height - 4.0 - bh),
                        Size::new((x1 - x0 - 2.0).max(1.0), bh),
                        Color { a: 0.35, ..data.amber },
                    );
                }
            }
            // Events with known dates.
            for e in &self.t.events {
                let Some(s) = e.starts_at else { continue };
                let x = self.x(size, s);
                if let Some(end) = e.ends_at {
                    let x2 = self.x(size, end);
                    frame.fill_rectangle(
                        Point::new(x, 14.0),
                        Size::new((x2 - x).max(2.0), 6.0),
                        Color { a: 0.5, ..data.clay },
                    );
                }
                let d = Path::new(|b| {
                    b.move_to(Point::new(x, 10.0));
                    b.line_to(Point::new(x + 6.0, 17.0));
                    b.line_to(Point::new(x, 24.0));
                    b.line_to(Point::new(x - 6.0, 17.0));
                    b.close();
                });
                frame.fill(&d, data.clay);
                frame.fill_text(Text {
                    content: e.title.chars().take(30).collect(),
                    position: Point::new(x + 8.0, 26.0),
                    color: pal.text,
                    size: SIZE_SMALL.into(),
                    ..Text::default()
                });
            }
            let points = self.points(size);
            // Story threads: connect members in time order; ring the first mention.
            let mut threads: HashMap<i64, Vec<usize>> = HashMap::new();
            for (i, a) in self.t.articles.iter().enumerate() {
                if let Some(c) = a.cluster {
                    threads.entry(c).or_default().push(i);
                }
            }
            for members in threads.values() {
                let pts: Vec<Point> = members.iter().filter_map(|i| points[*i]).collect();
                for w in pts.windows(2) {
                    frame.stroke(
                        &Path::line(w[0], w[1]),
                        Stroke::default().with_width(1.5).with_color(Color { a: 0.6, ..data.sage }),
                    );
                }
                if let Some(first) = pts.first() {
                    frame.stroke(&Path::circle(*first, 8.0), Stroke::default().with_width(2.0).with_color(data.sage));
                }
            }
            for (i, a) in self.t.articles.iter().enumerate() {
                let Some(p) = points[i] else { continue };
                let dot = Path::circle(p, if self.hover == Some(i) { 6.0 } else { 4.0 });
                if a.dated {
                    frame.fill(&dot, data.amber);
                } else {
                    frame.stroke(&dot, Stroke::default().with_width(1.5).with_color(data.amber));
                }
            }
            if let Some(i) = self.hover
                && let Some(p) = points.get(i).copied().flatten()
            {
                let a = &self.t.articles[i];
                let label = format!(
                    "{} — {}{}",
                    format::full_date(a.at),
                    a.title.chars().take(70).collect::<String>(),
                    if a.dated { "" } else { " (fetch time)" }
                );
                let x = (p.x + 10.0).min(size.width - 420.0).max(LEFT);
                frame.fill_rectangle(
                    Point::new(x - 4.0, p.y - 26.0),
                    Size::new(420.0, 20.0),
                    Color { a: 0.85, ..pal.background },
                );
                frame.fill_text(Text {
                    content: label,
                    position: Point::new(x, p.y - 24.0),
                    color: pal.text,
                    size: SIZE_SMALL.into(),
                    ..Text::default()
                });
            }
        });
        vec![geometry]
    }

    fn mouse_interaction(&self, st: &Pointer, _b: Rectangle, _c: mouse::Cursor) -> mouse::Interaction {
        if st.last.is_some() { mouse::Interaction::Pointer } else { mouse::Interaction::default() }
    }
}

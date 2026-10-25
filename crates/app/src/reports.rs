//! Reports screen: DuckDB analytics over a read-only snapshot, with CSV export.

use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::{Space, button, column, container, row, rule, scrollable, text};
use iced::{Element, Fill, Font, Task as Cmd, font};
use juttusihti_analytics::export::{Cell, to_csv};
use juttusihti_analytics::{
    AnalyticsWorker, Bucket, CooccurrenceRow, CostRow, CrossRow, LabelRow, RelationshipStat, TrendRow, UsageRow,
    VolumeRow, Window,
};
use juttusihti_core::pricing::format_usd;

use crate::format;

#[derive(Debug, Clone, Default)]
pub struct Data {
    articles: usize,
    bucket_label: &'static str,
    volume: Vec<VolumeRow>,
    labels: Vec<LabelRow>,
    label_trend: Vec<TrendRow>,
    labels_by_source: Vec<CrossRow>,
    cooccurrence: Vec<CooccurrenceRow>,
    relevance: Vec<TrendRow>,
    relationships: Vec<RelationshipStat>,
    usage: Vec<UsageRow>,
    cost: Vec<CostRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Report {
    Volume,
    Labels,
    LabelTrend,
    LabelsBySource,
    Cooccurrence,
    Relevance,
    Relationships,
    Usage,
    Cost,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Load,
    Days(u32),
    Loaded(Result<Box<Data>, String>),
    Cancel,
    Export(Report),
    Exported(Result<Option<PathBuf>, String>),
}

pub struct State {
    worker: Option<Arc<AnalyticsWorker>>,
    days: u32,
    data: Option<Data>,
    busy: bool,
    message: Option<Result<String, String>>,
}

fn bucket_for(days: u32) -> (Bucket, &'static str) {
    match days {
        0..=31 => (Bucket::Day, "day"),
        32..=180 => (Bucket::Week, "week"),
        _ => (Bucket::Month, "month"),
    }
}

impl State {
    pub fn new(worker: Option<Arc<AnalyticsWorker>>) -> Self {
        Self { worker, days: 30, data: None, busy: false, message: None }
    }

    pub fn load(&mut self) -> Cmd<Msg> {
        let Some(worker) = self.worker.clone() else {
            return Cmd::done(Msg::Loaded(Err("Analytics is unavailable.".into())));
        };
        if self.busy {
            return Cmd::none();
        }
        self.busy = true;
        let days = self.days;
        Cmd::perform(
            async move {
                tokio::task::spawn_blocking(move || -> Result<Box<Data>, String> {
                    let stats = worker.refresh().map_err(|e| e.to_string())?.recv().map_err(|e| e.to_string())?;
                    let stats = stats.map_err(|e| e.to_string())?;
                    let now = chrono::Utc::now().timestamp();
                    let offset = i64::from(chrono::Local::now().offset().local_minus_utc());
                    let w = Window { from: now - i64::from(days) * 86_400, to: now + 1, utc_offset_secs: offset };
                    let (b, bucket_label) = bucket_for(days);
                    let rx = worker
                        .run(move |a| {
                            Ok(Data {
                                articles: a.stats().articles,
                                bucket_label,
                                volume: a.volume_by_source(w, b)?,
                                labels: a.label_distribution(w)?,
                                label_trend: a.label_trend(w, b, 6)?,
                                labels_by_source: a.labels_by_source(w, 8)?,
                                cooccurrence: a.entity_cooccurrence(w, 2, 25)?,
                                relevance: a.relevance_trend(w, b)?,
                                relationships: a.relationship_stats()?,
                                usage: a.usage_by_model()?,
                                cost: a.daily_cost(w)?,
                            })
                        })
                        .map_err(|e| e.to_string())?;
                    let mut data = rx.recv().map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
                    data.articles = stats.articles;
                    Ok(Box::new(data))
                })
                .await
                .map_err(|e| e.to_string())?
            },
            Msg::Loaded,
        )
    }

    pub fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Load => return self.load(),
            Msg::Days(d) => {
                self.days = d;
                return self.load();
            }
            Msg::Loaded(r) => {
                self.busy = false;
                match r {
                    Ok(d) => {
                        self.data = Some(*d);
                        self.message = None;
                    }
                    Err(e) => self.message = Some(Err(e)),
                }
            }
            Msg::Cancel => {
                if let Some(w) = &self.worker {
                    w.cancel_handle().cancel();
                }
            }
            Msg::Export(report) => {
                let Some(data) = &self.data else { return Cmd::none() };
                let (name, csv) = export(data, report);
                return Cmd::perform(
                    async move {
                        let Some(file) = rfd::AsyncFileDialog::new()
                            .add_filter("CSV", &["csv"])
                            .set_file_name(format!("juttusihti-{name}.csv"))
                            .save_file()
                            .await
                        else {
                            return Ok(None);
                        };
                        file.write(csv.as_bytes()).await.map_err(|e| e.to_string())?;
                        Ok(Some(file.path().to_path_buf()))
                    },
                    Msg::Exported,
                );
            }
            Msg::Exported(Ok(Some(p))) => self.message = Some(Ok(format!("Exported {}", p.display()))),
            Msg::Exported(Ok(None)) => {}
            Msg::Exported(Err(e)) => self.message = Some(Err(e)),
        }
        Cmd::none()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let bold = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
        let range = |d: u32, label: &'static str| {
            button(text(label).size(12))
                .style(if self.days == d { button::primary } else { button::secondary })
                .on_press(Msg::Days(d))
        };
        let mut header = row![
            text("Reports").size(24).font(bold),
            Space::new().width(Fill),
            range(7, "7 days"),
            range(30, "30 days"),
            range(90, "90 days"),
            range(365, "1 year"),
        ]
        .spacing(8)
        .align_y(iced::Center);
        header = if self.busy {
            header.push(button(text("Cancel").size(12)).style(button::danger).on_press(Msg::Cancel))
        } else {
            header.push(button(text("Rebuild").size(12)).style(button::secondary).on_press(Msg::Load))
        };
        let mut col = column![
            header,
            text("Computed by DuckDB on a read-only snapshot of the local database. Only confirmed labels and entities are counted unless noted.")
                .size(12),
        ]
        .spacing(16)
        .padding(24);
        match &self.message {
            Some(Ok(m)) => col = col.push(container(text(m).size(12)).padding(6).style(container::success)),
            Some(Err(m)) => col = col.push(container(text(m).size(12)).padding(6).style(container::danger)),
            None => {}
        }
        let Some(d) = &self.data else {
            return scrollable(col.push(text(if self.busy { "Building snapshot…" } else { "No data yet." }))).into();
        };
        col = col.push(text(format!("Snapshot of {} articles; trends per {}.", d.articles, d.bucket_label)).size(12));
        col = col.push(section("Articles per source", Report::Volume, volume_view(&d.volume)));
        col = col.push(section(
            "Topic evolution (top labels)",
            Report::LabelTrend,
            trend_view(&d.label_trend, d.bucket_label, false),
        ));
        col =
            col.push(section(
                "Labels",
                Report::Labels,
                lines(d.labels.iter().map(|l| {
                    format!("{} — {} confirmed, {} pending AI suggestions", l.label, l.confirmed, l.suggested)
                })),
            ));
        col = col.push(section(
            "Labels by source",
            Report::LabelsBySource,
            lines(d.labels_by_source.iter().map(|c| format!("{} · {}: {}", c.label, c.source, c.articles))),
        ));
        col = col.push(section(
            "Entities appearing together",
            Report::Cooccurrence,
            lines(d.cooccurrence.iter().map(|c| format!("{} + {}: {} articles", c.a, c.b, c.articles))),
        ));
        col = col.push(section(
            "Relevance and clickbait",
            Report::Relevance,
            trend_view(&d.relevance, d.bucket_label, true),
        ));
        col = col.push(section(
            "Story links",
            Report::Relationships,
            lines(
                d.relationships
                    .iter()
                    .map(|r| format!("{} · {} · {}: {}", r.kind.replace('_', " "), r.origin, r.status, r.links)),
            ),
        ));
        col = col.push(section(
            "AI usage by model (all time)",
            Report::Usage,
            lines(d.usage.iter().map(|u| {
                format!(
                    "{} {} — {} calls, {} in ({} cached), {} out, {}",
                    u.provider,
                    u.model,
                    u.calls,
                    u.input_tokens,
                    u.cached_input_tokens,
                    u.output_tokens,
                    u.cost_microunits.map(|c| format_usd(c.max(0) as u64)).unwrap_or_else(|| "cost unknown".into())
                )
            })),
        ));
        col = col.push(section(
            "AI cost per day",
            Report::Cost,
            lines(d.cost.iter().map(|c| {
                format!(
                    "{} — {} calls, {} tokens, {}",
                    format::local(c.day_start).map(|d| d.format("%-d.%-m.%Y").to_string()).unwrap_or_default(),
                    c.calls,
                    c.tokens,
                    c.cost_microunits.map(|v| format_usd(v.max(0) as u64)).unwrap_or_else(|| "cost unknown".into())
                )
            })),
        ));
        scrollable(col).height(Fill).into()
    }
}

fn section<'a>(title: &'a str, report: Report, body: Element<'a, Msg>) -> Element<'a, Msg> {
    let bold = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
    column![
        row![
            text(title).size(17).font(bold),
            Space::new().width(Fill),
            button(text("Export CSV").size(11)).style(button::text).on_press(Msg::Export(report)),
        ]
        .align_y(iced::Center),
        body,
        rule::horizontal(1),
    ]
    .spacing(6)
    .into()
}

fn lines<'a>(items: impl Iterator<Item = String>) -> Element<'a, Msg> {
    let mut col = column![].spacing(2);
    let mut any = false;
    for l in items {
        any = true;
        col = col.push(text(l).size(13));
    }
    if !any {
        col = col.push(text("No data in this period.").size(12));
    }
    col.into()
}

fn volume_view(rows: &[VolumeRow]) -> Element<'_, Msg> {
    let mut totals: Vec<(String, i64)> = Vec::new();
    for v in rows {
        match totals.iter_mut().find(|(t, _)| *t == v.feed_title) {
            Some((_, n)) => *n += v.articles,
            None => totals.push((v.feed_title.clone(), v.articles)),
        }
    }
    totals.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let max = totals.first().map_or(1, |(_, n)| *n).max(1);
    let mut col = column![].spacing(4);
    for (title, n) in totals {
        let w = (n as f32 / max as f32 * 320.0).max(2.0);
        col = col.push(
            row![
                text(title).size(13).width(240),
                container(Space::new().width(w).height(12)).style(container::primary),
                text(n.to_string()).size(13),
            ]
            .spacing(8)
            .align_y(iced::Center),
        );
    }
    if rows.is_empty() {
        col = col.push(text("No articles in this period.").size(12));
    }
    col.into()
}

/// Compact trend table: one line per key, counts per bucket in order.
fn trend_view(rows: &[TrendRow], bucket: &str, show_confirmed: bool) -> Element<'static, Msg> {
    let mut keys: Vec<String> = Vec::new();
    for r in rows {
        if !keys.contains(&r.key) {
            keys.push(r.key.clone());
        }
    }
    let mut col = column![].spacing(2);
    for k in keys {
        let series: Vec<String> = rows
            .iter()
            .filter(|r| r.key == k)
            .map(|r| {
                let date = format::local(r.bucket_start).map(|d| d.format("%-d.%-m.").to_string()).unwrap_or_default();
                if show_confirmed {
                    format!("{date} {} ({} confirmed)", r.articles, r.confirmed)
                } else {
                    format!("{date} {}", r.articles)
                }
            })
            .collect();
        col = col.push(text(format!("{} — {}", k.replace('_', " "), series.join(" · "))).size(13));
    }
    if rows.is_empty() {
        col = col.push(text(format!("No data in this period (per {bucket}).")).size(12));
    }
    col.into()
}

fn iso_day(ts: i64) -> String {
    format::local(ts).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

/// Builds the CSV for one report: (file name part, contents).
fn export(d: &Data, report: Report) -> (&'static str, String) {
    match report {
        Report::Volume => (
            "volume",
            to_csv(
                &["bucket_start", "source", "articles"],
                d.volume
                    .iter()
                    .map(|v| vec![iso_day(v.bucket_start).into(), v.feed_title.clone().into(), v.articles.into()]),
            ),
        ),
        Report::Labels => (
            "labels",
            to_csv(
                &["label", "confirmed", "suggested"],
                d.labels.iter().map(|l| vec![l.label.clone().into(), l.confirmed.into(), l.suggested.into()]),
            ),
        ),
        Report::LabelTrend => (
            "topic-evolution",
            to_csv(
                &["bucket_start", "label", "articles"],
                d.label_trend
                    .iter()
                    .map(|t| vec![iso_day(t.bucket_start).into(), t.key.clone().into(), t.articles.into()]),
            ),
        ),
        Report::LabelsBySource => (
            "labels-by-source",
            to_csv(
                &["label", "source", "articles"],
                d.labels_by_source
                    .iter()
                    .map(|c| vec![c.label.clone().into(), c.source.clone().into(), c.articles.into()]),
            ),
        ),
        Report::Cooccurrence => (
            "entity-cooccurrence",
            to_csv(
                &["entity_a", "entity_b", "articles"],
                d.cooccurrence.iter().map(|c| vec![c.a.clone().into(), c.b.clone().into(), c.articles.into()]),
            ),
        ),
        Report::Relevance => (
            "relevance",
            to_csv(
                &["bucket_start", "class", "articles", "confirmed"],
                d.relevance.iter().map(|t| {
                    vec![iso_day(t.bucket_start).into(), t.key.clone().into(), t.articles.into(), t.confirmed.into()]
                }),
            ),
        ),
        Report::Relationships => (
            "story-links",
            to_csv(
                &["kind", "origin", "status", "links"],
                d.relationships.iter().map(|r| {
                    vec![r.kind.clone().into(), r.origin.clone().into(), r.status.clone().into(), r.links.into()]
                }),
            ),
        ),
        Report::Usage => (
            "ai-usage",
            to_csv(
                &[
                    "provider",
                    "model",
                    "calls",
                    "input_tokens",
                    "cached_input_tokens",
                    "output_tokens",
                    "cost_usd_micro",
                ],
                d.usage.iter().map(|u| {
                    vec![
                        u.provider.clone().into(),
                        u.model.clone().into(),
                        u.calls.into(),
                        u.input_tokens.into(),
                        u.cached_input_tokens.into(),
                        u.output_tokens.into(),
                        Cell::from(u.cost_microunits),
                    ]
                }),
            ),
        ),
        Report::Cost => (
            "ai-cost-per-day",
            to_csv(
                &["day", "calls", "tokens", "cost_usd_micro"],
                d.cost.iter().map(|c| {
                    vec![iso_day(c.day_start).into(), c.calls.into(), c.tokens.into(), Cell::from(c.cost_microunits)]
                }),
            ),
        ),
    }
}

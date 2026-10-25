//! Review screen: confirm or reject what an analysis session suggested.

use iced::widget::{Space, button, checkbox, column, container, pick_list, row, scrollable, text};
use iced::{Element, Fill, Font, Length, Task as Cmd, font};
use juttusihti_core::analysis::SessionSummary;
use juttusihti_core::review::{MentionRow, RelationshipRow, RelevanceRow, ReviewCounts, ReviewKind};
use juttusihti_core::{ArticleId, RelationshipKind, Status};

use crate::db::Db;

const LIMIT: u32 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Entities,
    Relationships,
    Relevance,
    Summary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionChoice(pub i64, pub String);

impl std::fmt::Display for SessionChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindChoice(pub RelationshipKind);

impl KindChoice {
    pub const ALL: [KindChoice; 6] = [
        KindChoice(RelationshipKind::Duplicate),
        KindChoice(RelationshipKind::SameEvent),
        KindChoice(RelationshipKind::FollowUp),
        KindChoice(RelationshipKind::Related),
        KindChoice(RelationshipKind::Citation),
        KindChoice(RelationshipKind::ConflictingClaim),
    ];
}

impl std::fmt::Display for KindChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(kind_label(self.0))
    }
}

pub fn kind_label(k: RelationshipKind) -> &'static str {
    match k {
        RelationshipKind::Duplicate => "Duplicate",
        RelationshipKind::SameEvent => "Same event",
        RelationshipKind::FollowUp => "Follow-up",
        RelationshipKind::Related => "Related",
        RelationshipKind::Citation => "Citation",
        RelationshipKind::ConflictingClaim => "Conflicting claim",
    }
}

#[derive(Debug, Clone, Default)]
pub struct Data {
    counts: ReviewCounts,
    mentions: Vec<MentionRow>,
    relationships: Vec<RelationshipRow>,
    relevance: Vec<RelevanceRow>,
    summary: Option<SummaryView>,
}

#[derive(Debug, Clone)]
pub struct SummaryView {
    text: String,
    points: Vec<(String, Vec<(ArticleId, String)>)>,
    open_questions: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    SessionsLoaded(Result<Vec<SessionSummary>, String>),
    Select(SessionChoice),
    Tab(Tab),
    ShowAll(bool),
    Loaded(Result<Box<Data>, String>),
    Mention(i64, Status),
    Relationship(i64, Status),
    Retype(i64, KindChoice),
    Relevance(i64, Status),
    Bulk(ReviewKind, Status),
    Done(Result<(), String>),
    /// Handled by the app: show the article in the reader.
    OpenArticle(ArticleId),
}

pub struct State {
    db: Db,
    sessions: Vec<SessionSummary>,
    session: Option<i64>,
    tab: Tab,
    show_all: bool,
    data: Data,
    error: Option<String>,
}

impl State {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            sessions: Vec::new(),
            session: None,
            tab: Tab::Entities,
            show_all: false,
            data: Data::default(),
            error: None,
        }
    }

    pub fn load(&self) -> Cmd<Msg> {
        let db = self.db.clone();
        let list = Cmd::perform(async move { db.call(|s| s.recent_sessions(50)).await }, Msg::SessionsLoaded);
        Cmd::batch([list, self.reload()])
    }

    /// Opens a session's review directly (e.g. after a run finishes).
    pub fn open(&mut self, session: i64) -> Cmd<Msg> {
        self.session = Some(session);
        self.load()
    }

    fn reload(&self) -> Cmd<Msg> {
        let Some(id) = self.session else { return Cmd::none() };
        let (db, pending) = (self.db.clone(), !self.show_all);
        Cmd::perform(
            async move {
                db.call(move |s| -> juttusihti_core::Result<Box<Data>> {
                    let summary = s.session_summary(id)?.map(|v| {
                        let points = v["key_points"]
                            .as_array()
                            .map(Vec::as_slice)
                            .unwrap_or_default()
                            .iter()
                            .map(|p| {
                                let ids: Vec<i64> = p["article_ids"]
                                    .as_array()
                                    .map(Vec::as_slice)
                                    .unwrap_or_default()
                                    .iter()
                                    .filter_map(serde_json::Value::as_i64)
                                    .collect();
                                (p["text"].as_str().unwrap_or_default().to_string(), ids)
                            })
                            .collect::<Vec<_>>();
                        (v, points)
                    });
                    let summary = match summary {
                        Some((v, points)) => {
                            let mut resolved = Vec::new();
                            for (t, ids) in points {
                                resolved.push((t, s.article_titles(&ids)?));
                            }
                            Some(SummaryView {
                                text: v["summary"].as_str().unwrap_or_default().to_string(),
                                points: resolved,
                                open_questions: v["open_questions"]
                                    .as_array()
                                    .map(Vec::as_slice)
                                    .unwrap_or_default()
                                    .iter()
                                    .filter_map(|q| q.as_str().map(str::to_string))
                                    .collect(),
                            })
                        }
                        None => None,
                    };
                    Ok(Box::new(Data {
                        counts: s.review_counts(id)?,
                        mentions: s.session_mentions(id, pending, LIMIT)?,
                        relationships: s.session_relationships(id, pending, LIMIT)?,
                        relevance: s.session_relevance(id, pending, LIMIT)?,
                        summary,
                    }))
                })
                .await
            },
            Msg::Loaded,
        )
    }

    fn act(
        &self,
        f: impl FnOnce(&mut juttusihti_core::store::Store) -> juttusihti_core::Result<()> + Send + 'static,
    ) -> Cmd<Msg> {
        let db = self.db.clone();
        Cmd::perform(async move { db.call(f).await }, Msg::Done)
    }

    pub fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::SessionsLoaded(Ok(s)) => {
                self.sessions = s;
                if self.session.is_none()
                    && let Some(first) = self.sessions.iter().find(|s| s.status.as_str() != "draft")
                {
                    self.session = Some(first.id);
                    return self.reload();
                }
            }
            Msg::SessionsLoaded(Err(e)) | Msg::Loaded(Err(e)) | Msg::Done(Err(e)) => self.error = Some(e),
            Msg::Select(SessionChoice(id, _)) => {
                self.session = Some(id);
                return self.reload();
            }
            Msg::Tab(t) => self.tab = t,
            Msg::ShowAll(on) => {
                self.show_all = on;
                return self.reload();
            }
            Msg::Loaded(Ok(d)) => {
                self.data = *d;
                self.error = None;
            }
            Msg::Mention(id, st) => return self.act(move |s| s.decide_mention(id, st)),
            Msg::Relationship(id, st) => return self.act(move |s| s.decide_relationship(id, st)),
            Msg::Retype(id, KindChoice(k)) => return self.act(move |s| s.retype_relationship(id, k)),
            Msg::Relevance(id, st) => return self.act(move |s| s.decide_relevance(id, st)),
            Msg::Bulk(kind, st) => {
                let Some(session) = self.session else { return Cmd::none() };
                return self.act(move |s| s.bulk_decide_session(session, kind, st).map(|_| ()));
            }
            Msg::Done(Ok(())) => return self.reload(),
            Msg::OpenArticle(_) => {}
        }
        Cmd::none()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let bold = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
        let choices: Vec<SessionChoice> = self
            .sessions
            .iter()
            .map(|s| {
                SessionChoice(s.id, format!("#{} · {} · {} · {}", s.id, s.status.as_str(), s.range_label, s.model))
            })
            .collect();
        let selected = choices.iter().find(|c| Some(c.0) == self.session).cloned();
        let c = self.data.counts;
        let tab = |t: Tab, label: String| {
            button(text(label).size(13))
                .style(if self.tab == t { button::primary } else { button::secondary })
                .on_press(Msg::Tab(t))
        };
        let header = column![
            text("Review AI suggestions").size(24).font(bold),
            text("Nothing here is treated as fact until you confirm it. Rejected items stay recorded but hidden.")
                .size(12),
            row![
                pick_list(choices, selected, Msg::Select).placeholder("Choose a session"),
                checkbox(self.show_all).label("Show decided items").on_toggle(Msg::ShowAll),
            ]
            .spacing(12)
            .align_y(iced::Center),
            row![
                tab(Tab::Entities, format!("Entities ({})", c.entities)),
                tab(Tab::Relationships, format!("Relationships ({})", c.relationships)),
                tab(Tab::Relevance, format!("Relevance ({})", c.relevance)),
                tab(Tab::Summary, if c.has_summary { "Summary".into() } else { "Summary (none)".into() }),
            ]
            .spacing(6),
        ]
        .spacing(10);

        let body: Element<'_, Msg> = if self.session.is_none() {
            text("Run an analysis first; its suggestions appear here.").size(14).into()
        } else {
            match self.tab {
                Tab::Entities => self.view_entities(),
                Tab::Relationships => self.view_relationships(),
                Tab::Relevance => self.view_relevance(),
                Tab::Summary => self.view_summary(),
            }
        };
        let mut col = column![header, body].spacing(16).padding(24);
        if let Some(e) = &self.error {
            col = col.push(container(text(e).size(12)).padding(8).style(container::danger));
        }
        scrollable(col).height(Fill).into()
    }

    fn view_entities(&self) -> Element<'_, Msg> {
        let mut col = column![bulk(ReviewKind::Entities, self.data.counts.entities)].spacing(4);
        for m in &self.data.mentions {
            col = col.push(item_row(
                column![
                    text(format!("{} · {} · {}", m.entity_name, m.kind.as_str(), m.role.replace('_', " "))).size(14),
                    article_link(m.article_id, &m.article_title),
                    evidence(m.evidence.as_deref()),
                ]
                .spacing(2)
                .into(),
                m.status,
                Msg::Mention(m.mention_id, Status::Confirmed),
                Msg::Mention(m.mention_id, Status::Rejected),
            ));
        }
        empty_hint(col, self.data.mentions.is_empty())
    }

    fn view_relationships(&self) -> Element<'_, Msg> {
        let mut col = column![bulk(ReviewKind::Relationships, self.data.counts.relationships)].spacing(4);
        for r in &self.data.relationships {
            let conf = r.confidence.map(|c| format!(" · model confidence {:.0}%", c * 100.0)).unwrap_or_default();
            col = col.push(item_row(
                column![
                    row![
                        pick_list(&KindChoice::ALL[..], Some(KindChoice(r.kind)), move |k| Msg::Retype(r.id, k)),
                        text(conf).size(11),
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                    article_link(r.source_id, &r.source_title),
                    article_link(r.target_id, &r.target_title),
                    evidence(r.evidence.as_deref()),
                ]
                .spacing(2)
                .into(),
                r.status,
                Msg::Relationship(r.id, Status::Confirmed),
                Msg::Relationship(r.id, Status::Rejected),
            ));
        }
        empty_hint(col, self.data.relationships.is_empty())
    }

    fn view_relevance(&self) -> Element<'_, Msg> {
        let mut col = column![
            bulk(ReviewKind::Relevance, self.data.counts.relevance),
            text("Confirmed unrelated, duplicate, low-information or clickbait articles can be hidden in the reader; they are never deleted.")
                .size(11),
        ]
        .spacing(4);
        for r in &self.data.relevance {
            col = col.push(item_row(
                column![
                    text(r.class.replace('_', " ")).size(14),
                    article_link(r.article_id, &r.article_title),
                    evidence(r.reason.as_deref()),
                ]
                .spacing(2)
                .into(),
                r.status,
                Msg::Relevance(r.id, Status::Confirmed),
                Msg::Relevance(r.id, Status::Rejected),
            ));
        }
        empty_hint(col, self.data.relevance.is_empty())
    }

    fn view_summary(&self) -> Element<'_, Msg> {
        let Some(s) = &self.data.summary else {
            return text("This session has no summary.").size(13).into();
        };
        let bold = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
        let mut col = column![
            text("AI-generated summary. Check the cited articles before relying on any point.").size(11),
            text(&s.text).size(15).line_height(1.5),
            text("Key points").size(16).font(bold),
        ]
        .spacing(8)
        .max_width(820);
        for (point, sources) in &s.points {
            let mut p = column![text(format!("• {point}")).size(14)].spacing(2);
            for (id, title) in sources {
                p = p.push(row![Space::new().width(16), article_link(*id, title)]);
            }
            col = col.push(p);
        }
        if !s.open_questions.is_empty() {
            col = col.push(text("Open questions").size(16).font(bold));
            for q in &s.open_questions {
                col = col.push(text(format!("• {q}")).size(14));
            }
        }
        col.into()
    }
}

fn bulk<'a>(kind: ReviewKind, pending: u64) -> Element<'a, Msg> {
    row![
        text(format!("{pending} pending")).size(12).width(Length::Fill),
        button(text("Confirm all pending").size(12))
            .style(button::success)
            .on_press_maybe((pending > 0).then_some(Msg::Bulk(kind, Status::Confirmed))),
        button(text("Reject all pending").size(12))
            .style(button::secondary)
            .on_press_maybe((pending > 0).then_some(Msg::Bulk(kind, Status::Rejected))),
    ]
    .spacing(8)
    .align_y(iced::Center)
    .into()
}

fn item_row<'a>(content: Element<'a, Msg>, status: Status, yes: Msg, no: Msg) -> Element<'a, Msg> {
    let decision: Element<'a, Msg> = match status {
        Status::Suggested => row![
            button(text("✓ Confirm").size(12)).style(button::success).on_press(yes),
            button(text("✕ Reject").size(12)).style(button::secondary).on_press(no),
        ]
        .spacing(6)
        .into(),
        Status::Confirmed => {
            row![text("Confirmed").size(12), button(text("Reject").size(11)).style(button::text).on_press(no)]
                .spacing(6)
                .into()
        }
        Status::Rejected => {
            row![text("Rejected").size(12), button(text("Confirm").size(11)).style(button::text).on_press(yes)]
                .spacing(6)
                .into()
        }
    };
    container(row![container(content).width(Fill), decision].spacing(12).align_y(iced::Center))
        .padding(8)
        .style(container::bordered_box)
        .into()
}

fn article_link<'a>(id: ArticleId, title: &str) -> Element<'a, Msg> {
    button(text(title.to_string()).size(12)).style(button::text).padding(0).on_press(Msg::OpenArticle(id)).into()
}

fn evidence<'a>(e: Option<&str>) -> Element<'a, Msg> {
    match e {
        Some(e) => text(format!("“{e}”")).size(11).into(),
        None => text("No quoted evidence").size(11).into(),
    }
}

fn empty_hint(col: iced::widget::Column<'_, Msg>, empty: bool) -> Element<'_, Msg> {
    if empty { col.push(text("Nothing to review here.").size(13)).into() } else { col.into() }
}

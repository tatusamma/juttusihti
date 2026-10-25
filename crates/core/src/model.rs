//! Plain data types shared by storage, search and the UI.

use serde::{Deserialize, Serialize};

pub type FeedId = i64;
pub type FolderId = i64;
pub type ArticleId = i64;
pub type LabelId = i64;
pub type EntityId = i64;
pub type EventId = i64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub id: FolderId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feed {
    pub id: FeedId,
    pub url: String,
    pub title: String,
    pub site_url: Option<String>,
    pub folder_id: Option<FolderId>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub last_fetched_at: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewFeed {
    pub url: String,
    pub title: String,
    pub site_url: Option<String>,
    pub folder_id: Option<FolderId>,
}

/// An entry parsed from a feed, before it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewArticle {
    pub guid: String,
    pub url: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Article {
    pub id: ArticleId,
    pub feed_id: FeedId,
    pub guid: String,
    pub url: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<i64>,
    pub fetched_at: i64,
    pub effective_at: i64,
    pub dedup_key: String,
    pub is_read: bool,
    pub is_saved: bool,
}

/// A row in an article list: enough to render without loading full content.
#[derive(Debug, Clone, PartialEq)]
pub struct ArticleSummary {
    pub id: ArticleId,
    pub feed_id: FeedId,
    pub feed_title: String,
    pub title: String,
    pub url: Option<String>,
    pub effective_at: i64,
    pub is_read: bool,
    pub is_saved: bool,
    /// Highlighted snippet when produced by a full-text search.
    pub snippet: Option<String>,
    /// FTS5 bm25 rank (lower is better) when produced by a search.
    pub rank: Option<f64>,
}

/// Which built-in collection an article list shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum View {
    #[default]
    All,
    Unread,
    Saved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub offset: u32,
    pub limit: u32,
}

impl Page {
    pub const fn first(limit: u32) -> Self {
        Self { offset: 0, limit }
    }
    pub const fn next(self) -> Self {
        Self { offset: self.offset + self.limit, limit: self.limit }
    }
    pub const fn prev(self) -> Self {
        Self { offset: self.offset.saturating_sub(self.limit), limit: self.limit }
    }
}

/// Result of storing a batch of parsed entries for one feed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IngestStats {
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
}

/// Who created a research record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    User,
    Ai,
}

/// Review state. AI output starts as `Suggested`; users confirm or reject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Suggested,
    Confirmed,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Person,
    Organization,
    Place,
    Concept,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipKind {
    Duplicate,
    SameEvent,
    FollowUp,
    Related,
    Citation,
    ConflictingClaim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocationRole {
    Event,
    EntityAssociated,
    Publisher,
    Mentioned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Draft,
    Estimated,
    AwaitingApproval,
    Running,
    Paused,
    Cancelled,
    Failed,
    Completed,
}

macro_rules! sql_enum {
    ($ty:ty { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $ty {
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($text => Some(Self::$variant),)+ _ => None }
            }
        }
        impl rusqlite::ToSql for $ty {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(self.as_str().into())
            }
        }
        impl rusqlite::types::FromSql for $ty {
            fn column_result(v: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
                let s = v.as_str()?;
                Self::parse(s).ok_or_else(|| rusqlite::types::FromSqlError::Other(
                    format!("unknown {} value {s:?}", stringify!($ty)).into()))
            }
        }
    };
}

sql_enum!(Origin { User => "user", Ai => "ai" });
sql_enum!(Status { Suggested => "suggested", Confirmed => "confirmed", Rejected => "rejected" });
sql_enum!(EntityKind {
    Person => "person", Organization => "organization", Place => "place",
    Concept => "concept", Other => "other",
});
sql_enum!(RelationshipKind {
    Duplicate => "duplicate", SameEvent => "same_event", FollowUp => "follow_up",
    Related => "related", Citation => "citation", ConflictingClaim => "conflicting_claim",
});
sql_enum!(LocationRole {
    Event => "event", EntityAssociated => "entity_associated",
    Publisher => "publisher", Mentioned => "mentioned",
});
sql_enum!(SessionStatus {
    Draft => "draft", Estimated => "estimated", AwaitingApproval => "awaiting_approval",
    Running => "running", Paused => "paused", Cancelled => "cancelled",
    Failed => "failed", Completed => "completed",
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub id: LabelId,
    pub name: String,
    pub color: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticleLabel {
    pub label: Label,
    pub origin: Origin,
    pub status: Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entity {
    pub id: EntityId,
    pub kind: EntityKind,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Relationship {
    pub id: i64,
    pub source_id: ArticleId,
    pub target_id: ArticleId,
    pub kind: RelationshipKind,
    pub origin: Origin,
    pub status: Status,
    pub evidence: Option<String>,
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: EventId,
    pub title: String,
    pub description: Option<String>,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
    pub origin: Origin,
    pub status: Status,
}

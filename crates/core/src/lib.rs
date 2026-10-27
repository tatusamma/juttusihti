//! Juttusihti core: domain model, SQLite storage, feed parsing, OPML,
//! full-text search and the research data model.
//!
//! Nothing in this crate needs network access or an AI provider.

pub mod ai_defaults;
pub mod analysis;
pub mod budget;
pub mod db;
pub mod discovery;
pub mod estimate;
pub mod feed;
pub mod graph;
pub mod headlines;
pub mod model;
pub mod normalize;
pub mod opml_io;
pub mod pricing;
pub mod research;
pub mod review;
pub mod search;
pub mod store;
pub mod timeline;

pub use model::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database schema version {found} is newer than this build supports ({supported})")]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("feed parse error: {0}")]
    FeedParse(String),
    #[error("OPML error: {0}")]
    Opml(String),
    #[error("invalid search query: {0}")]
    Query(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("not found")]
    NotFound,
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Current UTC time as unix seconds.
pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

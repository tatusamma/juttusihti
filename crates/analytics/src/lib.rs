//! DuckDB analytics over a consistent snapshot of the SQLite database.
//!
//! SQLite stays the source of truth. Instead of the DuckDB `sqlite`
//! extension (not bundled by duckdb-rs, and only installable from the
//! network at runtime), the relevant tables are copied inside one SQLite
//! read transaction into an in-memory DuckDB database. That gives a
//! consistent snapshot, works offline and never holds a SQLite write lock.
//!
//! The DuckDB instance is locked down: no extension autoload/autoinstall,
//! no external file or network access, bounded memory and threads, and the
//! configuration is locked so later SQL cannot loosen it. Only the fixed
//! report queries in this crate run; there is no free-form SQL entry point.

pub mod export;
mod worker;

pub use worker::{AnalyticsWorker, CancelHandle};

use duckdb::{Config, params};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("DuckDB error: {0}")]
    DuckDb(#[from] duckdb::Error),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("query was cancelled")]
    Cancelled,
    #[error("analytics worker stopped")]
    WorkerGone,
    #[error("analytics queue is full")]
    Busy,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct Limits {
    pub memory: String,
    pub threads: i64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { memory: "512MB".into(), threads: 2 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    Day,
    Week,
    Month,
}

impl Bucket {
    fn sql(self) -> &'static str {
        match self {
            Bucket::Day => "day",
            Bucket::Week => "week",
            Bucket::Month => "month",
        }
    }
}

/// Half-open time window `[from, to)` in UTC unix seconds, plus a fixed
/// UTC offset used for bucketing. (DST-aware zones would need the ICU
/// extension, which is not bundled.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub from: i64,
    pub to: i64,
    pub utc_offset_secs: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VolumeRow {
    pub feed_id: i64,
    pub feed_title: String,
    /// Bucket start, UTC unix seconds.
    pub bucket_start: i64,
    pub articles: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LabelRow {
    pub label: String,
    pub confirmed: i64,
    pub suggested: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CooccurrenceRow {
    pub a: String,
    pub b: String,
    pub articles: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UsageRow {
    pub provider: String,
    pub model: String,
    pub calls: i64,
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    /// `None` when any contributing call had unknown cost.
    pub cost_microunits: Option<i64>,
}

/// A count per key and time bucket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrendRow {
    pub key: String,
    /// Bucket start, UTC unix seconds.
    pub bucket_start: i64,
    pub articles: i64,
    pub confirmed: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossRow {
    pub source: String,
    pub label: String,
    pub articles: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationshipStat {
    pub kind: String,
    pub origin: String,
    pub status: String,
    pub links: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostRow {
    pub day_start: i64,
    pub calls: i64,
    pub tokens: i64,
    pub cost_microunits: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SnapshotStats {
    pub feeds: usize,
    pub articles: usize,
}

/// An in-memory DuckDB database holding one snapshot.
pub struct Analytics {
    conn: duckdb::Connection,
    stats: SnapshotStats,
}

const SCHEMA: &str = "
CREATE TABLE feeds (id BIGINT PRIMARY KEY, title VARCHAR NOT NULL, folder_id BIGINT);
CREATE TABLE articles (id BIGINT PRIMARY KEY, feed_id BIGINT NOT NULL, effective_at BIGINT NOT NULL,
                       is_read BOOLEAN NOT NULL, is_saved BOOLEAN NOT NULL, dedup_key VARCHAR NOT NULL);
CREATE TABLE labels (id BIGINT PRIMARY KEY, name VARCHAR NOT NULL);
CREATE TABLE article_labels (article_id BIGINT, label_id BIGINT, status VARCHAR);
CREATE TABLE entities (id BIGINT PRIMARY KEY, kind VARCHAR, name VARCHAR);
CREATE TABLE article_entities (article_id BIGINT, entity_id BIGINT, status VARCHAR);
CREATE TABLE usage_ledger (provider VARCHAR, model VARCHAR, operation VARCHAR, input_tokens BIGINT,
                           cached_input_tokens BIGINT, output_tokens BIGINT, reasoning_tokens BIGINT,
                           cost_microunits BIGINT, created_at BIGINT);
CREATE TABLE article_relevance (article_id BIGINT, class VARCHAR, status VARCHAR);
CREATE TABLE article_relationships (source_id BIGINT, target_id BIGINT, kind VARCHAR, origin VARCHAR, status VARCHAR);
";

/// Opens a locked-down in-memory DuckDB instance.
pub fn locked_connection(limits: &Limits) -> Result<duckdb::Connection> {
    let config = Config::default()
        .enable_autoload_extension(false)?
        .enable_external_access(false)?
        .max_memory(&limits.memory)?
        .threads(limits.threads)?
        .with("autoinstall_known_extensions", "false")?
        .with("allow_community_extensions", "false")?;
    let conn = duckdb::Connection::open_in_memory_with_flags(config)?;
    conn.execute_batch("SET lock_configuration = true;")?;
    Ok(conn)
}

impl Analytics {
    /// Copies the analytical tables from SQLite inside one read transaction.
    pub fn snapshot(sqlite: &rusqlite::Connection, limits: &Limits) -> Result<Self> {
        let conn = locked_connection(limits)?;
        conn.execute_batch(SCHEMA)?;
        let tx = sqlite.unchecked_transaction()?;
        let feeds = copy(&tx, &conn, "feeds", "SELECT id, title, folder_id FROM feeds", |r, a| {
            Ok(a.append_row(params![r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?])?)
        })?;
        let articles = copy(
            &tx,
            &conn,
            "articles",
            "SELECT id, feed_id, effective_at, is_read, is_saved, dedup_key FROM articles",
            |r, a| {
                Ok(a.append_row(params![
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, bool>(3)?,
                    r.get::<_, bool>(4)?,
                    r.get::<_, String>(5)?
                ])?)
            },
        )?;
        copy(&tx, &conn, "labels", "SELECT id, name FROM labels", |r, a| {
            Ok(a.append_row(params![r.get::<_, i64>(0)?, r.get::<_, String>(1)?])?)
        })?;
        copy(&tx, &conn, "article_labels", "SELECT article_id, label_id, status FROM article_labels", |r, a| {
            Ok(a.append_row(params![r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?])?)
        })?;
        copy(&tx, &conn, "entities", "SELECT id, kind, name FROM entities", |r, a| {
            Ok(a.append_row(params![r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?])?)
        })?;
        copy(&tx, &conn, "article_entities", "SELECT article_id, entity_id, status FROM article_entities", |r, a| {
            Ok(a.append_row(params![r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?])?)
        })?;
        copy(
            &tx,
            &conn,
            "usage_ledger",
            "SELECT provider, model, operation, input_tokens, cached_input_tokens, output_tokens, reasoning_tokens,
                    cost_microunits, created_at FROM usage_ledger",
            |r, a| {
                Ok(a.append_row(params![
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                    r.get::<_, i64>(8)?
                ])?)
            },
        )?;
        copy(&tx, &conn, "article_relevance", "SELECT article_id, class, status FROM article_relevance", |r, a| {
            Ok(a.append_row(params![r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?])?)
        })?;
        copy(
            &tx,
            &conn,
            "article_relationships",
            "SELECT source_id, target_id, kind, origin, status FROM article_relationships",
            |r, a| {
                Ok(a.append_row(params![
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?
                ])?)
            },
        )?;
        tx.commit()?;
        let stats = SnapshotStats { feeds, articles };
        tracing::debug!(?stats, "analytics snapshot loaded");
        Ok(Self { conn, stats })
    }

    pub fn stats(&self) -> SnapshotStats {
        self.stats
    }

    pub fn interrupt_handle(&self) -> std::sync::Arc<duckdb::InterruptHandle> {
        self.conn.interrupt_handle()
    }

    /// Article volume per source and time bucket within a window.
    pub fn volume_by_source(&self, window: Window, bucket: Bucket) -> Result<Vec<VolumeRow>> {
        let sql = format!(
            "SELECT a.feed_id, f.title,
                    CAST(epoch(date_trunc('{b}', make_timestamp((a.effective_at + ?1) * 1000000))) AS BIGINT) - ?1 AS bucket,
                    count(*) AS n
             FROM articles a JOIN feeds f ON f.id = a.feed_id
             WHERE a.effective_at >= ?2 AND a.effective_at < ?3
             GROUP BY ALL ORDER BY bucket, f.title",
            b = bucket.sql()
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![window.utc_offset_secs, window.from, window.to], |r| {
            Ok(VolumeRow { feed_id: r.get(0)?, feed_title: r.get(1)?, bucket_start: r.get(2)?, articles: r.get(3)? })
        })?;
        Ok(rows.collect::<duckdb::Result<_>>()?)
    }

    /// Label usage, split by confirmed and pending AI suggestions.
    pub fn label_distribution(&self, window: Window) -> Result<Vec<LabelRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT l.name,
                    count(*) FILTER (WHERE al.status = 'confirmed') AS confirmed,
                    count(*) FILTER (WHERE al.status = 'suggested') AS suggested
             FROM article_labels al
             JOIN labels l ON l.id = al.label_id
             JOIN articles a ON a.id = al.article_id
             WHERE al.status <> 'rejected' AND a.effective_at >= ?1 AND a.effective_at < ?2
             GROUP BY l.name ORDER BY confirmed DESC, suggested DESC, l.name",
        )?;
        let rows = stmt.query_map(params![window.from, window.to], |r| {
            Ok(LabelRow { label: r.get(0)?, confirmed: r.get(1)?, suggested: r.get(2)? })
        })?;
        Ok(rows.collect::<duckdb::Result<_>>()?)
    }

    /// Pairs of confirmed entities that appear in the same articles.
    pub fn entity_cooccurrence(&self, window: Window, min_articles: i64, limit: i64) -> Result<Vec<CooccurrenceRow>> {
        let mut stmt = self.conn.prepare(
            "WITH m AS (
                SELECT DISTINCT ae.article_id, e.id, e.name FROM article_entities ae
                JOIN entities e ON e.id = ae.entity_id
                JOIN articles a ON a.id = ae.article_id
                WHERE ae.status = 'confirmed' AND a.effective_at >= ?1 AND a.effective_at < ?2)
             SELECT x.name, y.name, count(*) AS n
             FROM m x JOIN m y ON x.article_id = y.article_id AND x.id < y.id
             GROUP BY x.name, y.name HAVING count(*) >= ?3
             ORDER BY n DESC, x.name, y.name LIMIT ?4",
        )?;
        let rows = stmt.query_map(params![window.from, window.to, min_articles, limit], |r| {
            Ok(CooccurrenceRow { a: r.get(0)?, b: r.get(1)?, articles: r.get(2)? })
        })?;
        Ok(rows.collect::<duckdb::Result<_>>()?)
    }

    /// Token and cost totals per provider and model. Cost is `None` if any
    /// call's cost was unknown, so unknown never silently becomes zero.
    pub fn usage_by_model(&self) -> Result<Vec<UsageRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT provider, model, count(*), sum(input_tokens), sum(cached_input_tokens), sum(output_tokens + reasoning_tokens),
                    CASE WHEN count(*) = count(cost_microunits) THEN sum(cost_microunits) END
             FROM usage_ledger GROUP BY provider, model ORDER BY provider, model",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(UsageRow {
                provider: r.get(0)?,
                model: r.get(1)?,
                calls: r.get(2)?,
                input_tokens: r.get(3)?,
                cached_input_tokens: r.get(4)?,
                output_tokens: r.get(5)?,
                cost_microunits: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<duckdb::Result<_>>()?)
    }

    /// Number of distinct stories (dedup keys) vs. articles per source:
    /// a rough syndication indicator.
    pub fn distinct_stories(&self, window: Window) -> Result<Vec<(String, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.title, count(*), count(DISTINCT a.dedup_key) FROM articles a JOIN feeds f ON f.id = a.feed_id
             WHERE a.effective_at >= ?1 AND a.effective_at < ?2 GROUP BY f.title ORDER BY f.title",
        )?;
        let rows = stmt.query_map(params![window.from, window.to], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<duckdb::Result<_>>()?)
    }

    /// Label usage per time bucket for the `top` most used confirmed labels:
    /// how topics emerge and fade.
    pub fn label_trend(&self, window: Window, bucket: Bucket, top: i64) -> Result<Vec<TrendRow>> {
        let sql = format!(
            "WITH l AS (
                SELECT lb.name, a.effective_at FROM article_labels al
                JOIN labels lb ON lb.id = al.label_id JOIN articles a ON a.id = al.article_id
                WHERE al.status = 'confirmed' AND a.effective_at >= ?2 AND a.effective_at < ?3),
             top AS (SELECT name FROM l GROUP BY name ORDER BY count(*) DESC, name LIMIT ?4)
             SELECT l.name,
                    CAST(epoch(date_trunc('{b}', make_timestamp((l.effective_at + ?1) * 1000000))) AS BIGINT) - ?1 AS bucket,
                    count(*), count(*)
             FROM l JOIN top USING (name) GROUP BY ALL ORDER BY bucket, l.name",
            b = bucket.sql()
        );
        self.trend(&sql, params![window.utc_offset_secs, window.from, window.to, top])
    }

    /// Relevance classes per time bucket (all non-rejected; `confirmed`
    /// counts only user-confirmed ones), e.g. clickbait trends.
    pub fn relevance_trend(&self, window: Window, bucket: Bucket) -> Result<Vec<TrendRow>> {
        let sql = format!(
            "SELECT r.class,
                    CAST(epoch(date_trunc('{b}', make_timestamp((a.effective_at + ?1) * 1000000))) AS BIGINT) - ?1 AS bucket,
                    count(*), count(*) FILTER (WHERE r.status = 'confirmed')
             FROM article_relevance r JOIN articles a ON a.id = r.article_id
             WHERE r.status <> 'rejected' AND a.effective_at >= ?2 AND a.effective_at < ?3
             GROUP BY ALL ORDER BY bucket, r.class",
            b = bucket.sql()
        );
        self.trend(&sql, params![window.utc_offset_secs, window.from, window.to])
    }

    fn trend(&self, sql: &str, p: impl duckdb::Params) -> Result<Vec<TrendRow>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(p, |r| {
            Ok(TrendRow { key: r.get(0)?, bucket_start: r.get(1)?, articles: r.get(2)?, confirmed: r.get(3)? })
        })?;
        Ok(rows.collect::<duckdb::Result<_>>()?)
    }

    /// Confirmed labels per source: how coverage differs across sources.
    pub fn labels_by_source(&self, window: Window, top_labels: i64) -> Result<Vec<CrossRow>> {
        let mut stmt = self.conn.prepare(
            "WITH l AS (
                SELECT f.title AS source, lb.name AS label FROM article_labels al
                JOIN labels lb ON lb.id = al.label_id JOIN articles a ON a.id = al.article_id
                JOIN feeds f ON f.id = a.feed_id
                WHERE al.status = 'confirmed' AND a.effective_at >= ?1 AND a.effective_at < ?2),
             top AS (SELECT label FROM l GROUP BY label ORDER BY count(*) DESC, label LIMIT ?3)
             SELECT source, label, count(*) FROM l JOIN top USING (label)
             GROUP BY ALL ORDER BY label, count(*) DESC, source",
        )?;
        let rows = stmt.query_map(params![window.from, window.to, top_labels], |r| {
            Ok(CrossRow { source: r.get(0)?, label: r.get(1)?, articles: r.get(2)? })
        })?;
        Ok(rows.collect::<duckdb::Result<_>>()?)
    }

    /// Story-link counts by type, origin and review status.
    pub fn relationship_stats(&self) -> Result<Vec<RelationshipStat>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, origin, status, count(*) FROM article_relationships
             GROUP BY ALL ORDER BY kind, origin, status",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(RelationshipStat { kind: r.get(0)?, origin: r.get(1)?, status: r.get(2)?, links: r.get(3)? })
        })?;
        Ok(rows.collect::<duckdb::Result<_>>()?)
    }

    /// AI calls, tokens and cost per day. Cost is `None` for days with any
    /// unpriced call.
    pub fn daily_cost(&self, window: Window) -> Result<Vec<CostRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT CAST(epoch(date_trunc('day', make_timestamp((created_at + ?1) * 1000000))) AS BIGINT) - ?1 AS day,
                    count(*), sum(input_tokens + cached_input_tokens + output_tokens + reasoning_tokens),
                    CASE WHEN count(*) = count(cost_microunits) THEN sum(cost_microunits) END
             FROM usage_ledger WHERE created_at >= ?2 AND created_at < ?3
             GROUP BY ALL ORDER BY day",
        )?;
        let rows = stmt.query_map(params![window.utc_offset_secs, window.from, window.to], |r| {
            Ok(CostRow { day_start: r.get(0)?, calls: r.get(1)?, tokens: r.get(2)?, cost_microunits: r.get(3)? })
        })?;
        Ok(rows.collect::<duckdb::Result<_>>()?)
    }

    /// Runs a deliberately heavy query; used to test cancellation.
    #[doc(hidden)]
    pub fn heavy_query_for_tests(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM range(100000000) a, range(1000) b WHERE (a.range * b.range) % 7 = 3",
            [],
            |r| r.get(0),
        )?)
    }
}

fn copy(
    sqlite: &rusqlite::Connection,
    duck: &duckdb::Connection,
    table: &str,
    select: &str,
    mut append: impl FnMut(&rusqlite::Row<'_>, &mut duckdb::Appender<'_>) -> Result<()>,
) -> Result<usize> {
    let mut appender = duck.appender(table)?;
    let mut stmt = sqlite.prepare(select)?;
    let mut rows = stmt.query([])?;
    let mut n = 0;
    while let Some(row) = rows.next()? {
        append(row, &mut appender)?;
        n += 1;
    }
    appender.flush()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engines_link_together_and_report_versions() {
        let s = rusqlite::Connection::open_in_memory().unwrap();
        let sv: String = s.query_row("select sqlite_version()", [], |r| r.get(0)).unwrap();
        let d = locked_connection(&Limits::default()).unwrap();
        let dv: String = d.query_row("select version()", [], |r| r.get(0)).unwrap();
        println!("sqlite {sv}, duckdb {dv}");
        assert!(sv.starts_with('3'));
    }

    #[test]
    fn locked_connection_blocks_extensions_files_and_reconfiguration() {
        let d = locked_connection(&Limits::default()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.db").to_string_lossy().replace('\\', "/");
        for sql in [
            "INSTALL sqlite".to_string(),
            "LOAD sqlite".to_string(),
            format!("ATTACH '{path}' AS s (TYPE sqlite)"),
            format!("COPY (SELECT 1) TO '{path}.csv'"),
            format!("SELECT * FROM read_csv('{path}')"),
            "SET enable_external_access = true".to_string(),
            "SET autoinstall_known_extensions = true".to_string(),
        ] {
            assert!(d.execute_batch(&sql).is_err(), "should be blocked: {sql}");
        }
        let mem: String = d.query_row("SELECT current_setting('max_memory')", [], |r| r.get(0)).unwrap();
        assert!(mem.contains("MiB") || mem.contains("MB") || mem.contains("GiB"), "{mem}");
    }
}

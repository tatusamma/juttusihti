//! SQLite connection setup and schema migrations.
//!
//! SQLite is the source of truth. Migrations are applied in order inside a
//! single transaction each and tracked through `PRAGMA user_version`.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::Result;

const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_reader.sql"),
    include_str!("../migrations/0002_research.sql"),
    include_str!("../migrations/0003_relevance.sql"),
    include_str!("../migrations/0004_list_indexes.sql"),
    include_str!("../migrations/0005_headlines.sql"),
    include_str!("../migrations/0006_fulltext.sql"),
    include_str!("../migrations/0007_factcheck.sql"),
];

/// The schema version a fully migrated database reports.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// Opens (creating if needed) a read-write database and migrates it.
pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    configure(&conn)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    migrate(&conn)?;
    Ok(conn)
}

/// Opens an in-memory database, used by tests and previews.
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    configure(&conn)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Opens an existing database read-only, for analytics snapshots.
pub fn open_read_only(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

/// Applies any migrations newer than the database's `user_version`.
pub fn migrate(conn: &Connection) -> Result<()> {
    let current: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if current > SCHEMA_VERSION {
        return Err(crate::Error::SchemaTooNew { found: current, supported: SCHEMA_VERSION });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let version = index as u32 + 1;
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
        tracing::info!(version, "applied schema migration");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_to_latest_and_is_idempotent() {
        let conn = open_in_memory().unwrap();
        let v: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        migrate(&conn).unwrap();
        let fk: i64 = conn.pragma_query_value(None, "foreign_keys", |r| r.get(0)).unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn fulltext_migration_keeps_articles_and_rebuilds_the_index() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        for (i, sql) in MIGRATIONS[..5].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", i as u32 + 1).unwrap();
        }
        conn.execute_batch(
            "INSERT INTO feeds(id, url, title, created_at) VALUES (1, 'https://a.example/rss', 'A', 0);
             INSERT INTO articles(id, feed_id, guid, title, summary_text, fetched_at, effective_at, dedup_key, content_hash)
             VALUES (7, 1, 'g', 'Kuntavaalit lähestyvät', 'Ehdokkaita on paljon.', 0, 0, 'k', 'h');",
        )
        .unwrap();
        migrate(&conn).unwrap();
        let v: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let hit: i64 = conn
            .query_row("SELECT rowid FROM articles_fts WHERE articles_fts MATCH 'ehdokkaita'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hit, 7);
        conn.execute("INSERT INTO articles_fts(articles_fts, rank) VALUES ('integrity-check', 1)", []).unwrap();
    }

    #[test]
    fn rejects_newer_schema() {
        let conn = open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
        assert!(matches!(migrate(&conn), Err(crate::Error::SchemaTooNew { .. })));
    }

    #[test]
    fn file_database_uses_wal_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("j.db");
        {
            let conn = open(&path).unwrap();
            let mode: String = conn.pragma_query_value(None, "journal_mode", |r| r.get(0)).unwrap();
            assert_eq!(mode.to_lowercase(), "wal");
        }
        let conn = open(&path).unwrap();
        let v: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let ro = open_read_only(&path).unwrap();
        assert!(ro.execute("INSERT INTO folders(name, created_at) VALUES ('x', 0)", []).is_err());
    }
}

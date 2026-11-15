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
    include_str!("../migrations/0008_categories.sql"),
    include_str!("../migrations/0009_feed_categories.sql"),
    include_str!("../migrations/0010_file_publisher_terms.sql"),
    include_str!("../migrations/0011_settled_terms.sql"),
    include_str!("../migrations/0012_hidden_categories.sql"),
];

/// The schema version a fully migrated database reports.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// Opens (creating if needed) a read-write database and migrates it, first copying an
/// existing database that needs migrating (see [`backup_before_upgrade`]).
pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    configure(&conn)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    backup_before_upgrade(&conn, path)?;
    migrate(&conn)?;
    Ok(conn)
}

/// Copies an existing database that migrations are about to change to
/// `<name>-before-schema-<N>.db` next to it, N being the version it is upgraded to, so the
/// upgrade can be undone and an older build can still open the copy. A new or current database
/// is not copied. A copy that is already there is kept: it is the state before the first
/// attempt. When the copy cannot be made, the upgrade does not run.
fn backup_before_upgrade(conn: &Connection, path: &Path) -> Result<Option<std::path::PathBuf>> {
    let current: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if current == 0 || current >= SCHEMA_VERSION {
        return Ok(None);
    }
    let stem = path.file_stem().map_or_else(|| "database".into(), |s| s.to_string_lossy().into_owned());
    let backup = path.with_file_name(format!("{stem}-before-schema-{SCHEMA_VERSION}.db"));
    if backup.exists() {
        return Ok(Some(backup));
    }
    let target =
        backup.to_str().ok_or_else(|| crate::Error::Backup(format!("{} is not a valid path", backup.display())))?;
    conn.execute("VACUUM INTO ?1", [target])
        .map_err(|e| crate::Error::Backup(format!("could not copy the database to {}: {e}", backup.display())))?;
    tracing::info!(from = current, to = SCHEMA_VERSION, backup = %backup.display(), "backed up the database before upgrading it");
    Ok(Some(backup))
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
    fn categories_migration_leaves_labels_untouched_and_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v7.db");
        {
            let conn = Connection::open(&path).unwrap();
            configure(&conn).unwrap();
            for (i, sql) in MIGRATIONS[..7].iter().enumerate() {
                conn.execute_batch(sql).unwrap();
                conn.pragma_update(None, "user_version", i as u32 + 1).unwrap();
            }
            conn.execute_batch(
                "INSERT INTO feeds(id, url, title, created_at) VALUES (1, 'https://a.example/rss', 'A', 0);
                 INSERT INTO articles(id, feed_id, guid, title, summary_text, fetched_at, effective_at, dedup_key, content_hash)
                 VALUES (7, 1, 'g', 'Kuntavaalit', 'Ehdokkaita.', 0, 0, 'k', 'h');
                 INSERT INTO labels(id, name, normalized, created_at)
                 VALUES (3, 'Politiikka', 'politiikka', 0), (4, 'Sää', 'saa', 0);
                 INSERT INTO article_labels(article_id, label_id, origin, status, created_at)
                 VALUES (7, 3, 'user', 'confirmed', 0), (7, 4, 'ai', 'suggested', 0);",
            )
            .unwrap();
        }
        // The current code opens and upgrades it.
        let store = crate::store::Store::open(&path).unwrap();
        let conn = store.conn();
        let v: u32 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).unwrap();
        assert_eq!((v, SCHEMA_VERSION), (12, 12));
        let labels: Vec<(i64, String, Option<String>)> = conn
            .prepare("SELECT id, name, color FROM labels ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(labels, vec![(3, "Politiikka".into(), None), (4, "Sää".into(), None)]);
        let links: Vec<(i64, i64, String)> = conn
            .prepare("SELECT article_id, label_id, status FROM article_labels ORDER BY label_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(links, vec![(7, 3, "confirmed".into()), (7, 4, "suggested".into())]);
        let columns: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('labels') ORDER BY cid")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(columns, ["id", "name", "normalized", "color", "created_at"], "labels as on master");
        for table in [
            "category_groups",
            "categories",
            "feed_terms",
            "article_terms",
            "article_categories",
            "favourites",
            "category_ai_checks",
        ] {
            let n: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0)).unwrap();
            assert_eq!(n, 0, "{table} exists and starts empty");
        }
        let tree = store.category_tree().unwrap();
        assert!(tree.groups.is_empty() && tree.ungrouped.is_empty(), "labels are not categories");
        assert!(store.unsorted_terms().unwrap().is_empty());
    }

    #[test]
    fn publisher_terms_waiting_in_unsorted_are_filed_once() {
        let conn = Connection::open_in_memory().unwrap();
        configure(&conn).unwrap();
        for (i, sql) in MIGRATIONS[..9].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", i as u32 + 1).unwrap();
        }
        // Feeds 1 and 2 both send "Kotimaa" (in different case). "Urheilu" is already a
        // category in a group; "Talous" is filed elsewhere, "Tilaajille" ignored and "—" has
        // no letters. Kotimaa is on three articles, Ulkomaat on one.
        conn.execute_batch(
            "INSERT INTO feeds(id, url, title, created_at) VALUES (1, 'https://a/rss', 'A', 0), (2, 'https://b/rss', 'B', 0);
             INSERT INTO articles(id, feed_id, guid, title, fetched_at, effective_at, dedup_key, content_hash)
             VALUES (1, 1, 'a1', 'T', 0, 0, 'k1', 'h'), (2, 1, 'a2', 'T', 0, 0, 'k2', 'h'), (3, 2, 'b1', 'T', 0, 0, 'k3', 'h');
             INSERT INTO category_groups(id, name, normalized, position, created_at) VALUES (1, 'Sports', 'sports', 0, 0);
             INSERT INTO categories(id, name, normalized, group_id, position, created_at)
             VALUES (1, 'Urheilu', 'urheilu', 1, 0, 0), (2, 'Economy', 'economy', NULL, 0, 0);
             INSERT INTO feeds(id, url, title, created_at, default_category_id) VALUES (3, 'https://c/rss', 'C', 0, 2);
             INSERT INTO articles(id, feed_id, guid, title, fetched_at, effective_at, dedup_key, content_hash)
             VALUES (4, 3, 'c1', 'T', 0, 0, 'k4', 'h'), (5, 3, 'c2', 'T', 0, 0, 'k5', 'h');
             INSERT INTO feed_terms(id, feed_id, name, normalized, scheme, category_id, ignored, first_seen_at) VALUES
               (1, 1, 'Kotimaa', 'kotimaa', '', NULL, 0, 0), (2, 2, 'KOTIMAA', 'kotimaa', '', NULL, 0, 0),
               (3, 1, 'Ulkomaat', 'ulkomaat', '', NULL, 0, 0), (4, 1, 'urheilu', 'urheilu', '', NULL, 0, 0),
               (5, 1, 'Talous', 'talous', '', 2, 0, 0), (6, 1, 'Tilaajille', 'tilaajille', '', NULL, 1, 0),
               (7, 1, '—', '', '', NULL, 0, 0),
               (8, 3, 'Uutiset', 'uutiset', '', NULL, 0, 0), (9, 3, 'Urheilu', 'urheilu', '', NULL, 0, 0);
             INSERT INTO article_terms(article_id, term_id) VALUES (1, 1), (2, 1), (3, 2), (1, 3), (4, 8), (5, 8), (4, 9);",
        )
        .unwrap();
        migrate(&conn).unwrap();
        let filed = |id: i64| -> Option<i64> {
            conn.query_row("SELECT category_id FROM feed_terms WHERE id = ?1", [id], |r| r.get(0)).unwrap()
        };
        let category = |name: &str| -> (i64, Option<i64>, i64) {
            conn.query_row("SELECT id, group_id, position FROM categories WHERE name = ?1", [name], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap()
        };
        let kotimaa = category("Kotimaa");
        assert_eq!((kotimaa.1, kotimaa.2), (None, 1), "Ungrouped, after Economy");
        assert_eq!((filed(1), filed(2)), (Some(kotimaa.0), Some(kotimaa.0)), "one category for both feeds");
        assert_eq!(filed(3), None, "Ulkomaat is on one article: it waits in Unsorted");
        assert_eq!(filed(4), Some(1), "joins the existing category in its group, whatever its count");
        assert_eq!((filed(5), filed(6), filed(7)), (Some(2), None, None), "filed, ignored and nameless stay");
        assert_eq!((filed(8), filed(9)), (None, None), "feed 3 has a default category: its terms wait");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM categories", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 3);
        // Filed and ignored terms are settled; the waiting and nameless ones are not.
        let settled: Vec<i64> = conn
            .prepare("SELECT id FROM feed_terms WHERE settled = 1 ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(settled, [1, 2, 4, 5, 6]);
    }

    /// A database file at schema version `version`, with one feed.
    fn file_at_version(path: &Path, version: usize) {
        let conn = Connection::open(path).unwrap();
        configure(&conn).unwrap();
        for (i, sql) in MIGRATIONS[..version].iter().enumerate() {
            conn.execute_batch(sql).unwrap();
            conn.pragma_update(None, "user_version", i as u32 + 1).unwrap();
        }
        conn.execute("INSERT INTO feeds(id, url, title, created_at) VALUES (1, 'https://a/rss', 'A', 0)", []).unwrap();
    }

    fn version_of(path: &Path) -> u32 {
        Connection::open(path).unwrap().pragma_query_value(None, "user_version", |r| r.get(0)).unwrap()
    }

    #[test]
    fn upgrading_a_database_backs_it_up_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("juttusihti.db");
        file_at_version(&path, 9);
        drop(open(&path).unwrap());
        let backup = dir.path().join(format!("juttusihti-before-schema-{SCHEMA_VERSION}.db"));
        assert_eq!(version_of(&backup), 9, "the copy is the database as it was");
        let feeds: i64 =
            Connection::open(&backup).unwrap().query_row("SELECT COUNT(*) FROM feeds", [], |r| r.get(0)).unwrap();
        assert_eq!(feeds, 1);
        assert_eq!(version_of(&path), SCHEMA_VERSION);
    }

    #[test]
    fn a_new_or_current_database_gets_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("juttusihti.db");
        drop(open(&path).unwrap());
        drop(open(&path).unwrap());
        let files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("before-schema"))
            .collect();
        assert!(files.is_empty(), "{files:?}");
    }

    #[test]
    fn an_earlier_backup_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("juttusihti.db");
        file_at_version(&path, 9);
        let backup = dir.path().join(format!("juttusihti-before-schema-{SCHEMA_VERSION}.db"));
        std::fs::write(&backup, b"the first copy").unwrap();
        drop(open(&path).unwrap());
        assert_eq!(std::fs::read(&backup).unwrap(), b"the first copy");
        assert_eq!(version_of(&path), SCHEMA_VERSION);
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

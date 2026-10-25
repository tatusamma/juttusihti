-- Milestone 1: RSS reader. All timestamps are UTC unix seconds.
CREATE TABLE folders (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL
);

CREATE TABLE feeds (
    id              INTEGER PRIMARY KEY,
    url             TEXT NOT NULL UNIQUE,
    title           TEXT NOT NULL,
    site_url        TEXT,
    folder_id       INTEGER REFERENCES folders(id) ON DELETE SET NULL,
    etag            TEXT,
    last_modified   TEXT,
    last_fetched_at INTEGER,
    last_error      TEXT,
    created_at      INTEGER NOT NULL
);

-- Original source content. AI analysis never updates these columns.
CREATE TABLE articles (
    id           INTEGER PRIMARY KEY,
    feed_id      INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
    guid         TEXT NOT NULL,
    url          TEXT,
    title        TEXT NOT NULL,
    summary      TEXT,           -- original (may contain HTML)
    content      TEXT,           -- original (may contain HTML)
    summary_text TEXT,           -- plain text derived from summary, for search and AI input
    content_text TEXT,           -- plain text derived from content
    author       TEXT,
    published_at INTEGER,
    fetched_at   INTEGER NOT NULL,
    -- Sort and range key: published_at, falling back to fetched_at.
    effective_at INTEGER NOT NULL,
    dedup_key    TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    is_read      INTEGER NOT NULL DEFAULT 0 CHECK (is_read IN (0, 1)),
    is_saved     INTEGER NOT NULL DEFAULT 0 CHECK (is_saved IN (0, 1)),
    UNIQUE (feed_id, guid)
);
CREATE INDEX articles_effective_at ON articles(effective_at DESC);
CREATE INDEX articles_feed_effective ON articles(feed_id, effective_at DESC);
CREATE INDEX articles_dedup ON articles(dedup_key);
CREATE INDEX articles_saved ON articles(is_saved) WHERE is_saved = 1;
CREATE INDEX articles_unread ON articles(is_read) WHERE is_read = 0;

-- remove_diacritics 0: Finnish ä/ö/å are distinct letters, not accented a/o.
CREATE VIRTUAL TABLE articles_fts USING fts5(
    title, summary_text, content_text, author,
    content = 'articles', content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 0'
);

CREATE TRIGGER articles_ai AFTER INSERT ON articles BEGIN
    INSERT INTO articles_fts(rowid, title, summary_text, content_text, author)
    VALUES (new.id, new.title, new.summary_text, new.content_text, new.author);
END;
CREATE TRIGGER articles_ad AFTER DELETE ON articles BEGIN
    INSERT INTO articles_fts(articles_fts, rowid, title, summary_text, content_text, author)
    VALUES ('delete', old.id, old.title, old.summary_text, old.content_text, old.author);
END;
CREATE TRIGGER articles_au AFTER UPDATE OF title, summary_text, content_text, author ON articles BEGIN
    INSERT INTO articles_fts(articles_fts, rowid, title, summary_text, content_text, author)
    VALUES ('delete', old.id, old.title, old.summary_text, old.content_text, old.author);
    INSERT INTO articles_fts(rowid, title, summary_text, content_text, author)
    VALUES (new.id, new.title, new.summary_text, new.content_text, new.author);
END;

CREATE TABLE saved_searches (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    query_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

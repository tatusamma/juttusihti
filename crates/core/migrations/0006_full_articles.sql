-- Optional full-article fetching. Feeds opt in one by one; text extracted
-- from the article page is kept apart from the feed's own content, which is
-- never modified.
ALTER TABLE feeds ADD COLUMN fetch_full_text INTEGER NOT NULL DEFAULT 0 CHECK (fetch_full_text IN (0, 1));

-- One row per attempted article page. A failed or unhelpful attempt is kept
-- (status 'failed' / 'empty', text NULL) so the page is not fetched again.
CREATE TABLE article_extracts (
    article_id INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE,
    url        TEXT NOT NULL,      -- the page actually read, after redirects
    status     TEXT NOT NULL CHECK (status IN ('ok', 'empty', 'failed')),
    text       TEXT,               -- plain text of the main article body ('ok' only)
    text_hash  TEXT,               -- sha256 of text, part of AI cache keys
    error      TEXT,
    fetched_at INTEGER NOT NULL,
    CHECK ((status = 'ok') = (text IS NOT NULL))
);

-- The search index gains an extracted_text column. It stays an external
-- content index; its content source is a view over articles and extracts so
-- snippets can come from either.
DROP TRIGGER articles_ai;
DROP TRIGGER articles_ad;
DROP TRIGGER articles_au;
DROP TABLE articles_fts;

CREATE VIEW article_search_source AS
    SELECT a.id, a.title, a.summary_text, a.content_text, a.author, e.text AS extracted_text
    FROM articles a LEFT JOIN article_extracts e ON e.article_id = a.id;

CREATE VIRTUAL TABLE articles_fts USING fts5(
    title, summary_text, content_text, author, extracted_text,
    content = 'article_search_source', content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 0'
);

CREATE TRIGGER articles_ai AFTER INSERT ON articles BEGIN
    INSERT INTO articles_fts(rowid, title, summary_text, content_text, author, extracted_text)
    VALUES (new.id, new.title, new.summary_text, new.content_text, new.author,
            (SELECT text FROM article_extracts WHERE article_id = new.id));
END;
-- The extract goes first (its trigger re-indexes the article without it),
-- so the article's own 'delete' always sees extracted_text = NULL and the
-- cascade that follows has nothing left to do.
CREATE TRIGGER articles_bd BEFORE DELETE ON articles BEGIN
    DELETE FROM article_extracts WHERE article_id = old.id;
    INSERT INTO articles_fts(articles_fts, rowid, title, summary_text, content_text, author, extracted_text)
    VALUES ('delete', old.id, old.title, old.summary_text, old.content_text, old.author, NULL);
END;
CREATE TRIGGER articles_au AFTER UPDATE OF title, summary_text, content_text, author ON articles BEGIN
    INSERT INTO articles_fts(articles_fts, rowid, title, summary_text, content_text, author, extracted_text)
    VALUES ('delete', old.id, old.title, old.summary_text, old.content_text, old.author,
            (SELECT text FROM article_extracts WHERE article_id = old.id));
    INSERT INTO articles_fts(rowid, title, summary_text, content_text, author, extracted_text)
    VALUES (new.id, new.title, new.summary_text, new.content_text, new.author,
            (SELECT text FROM article_extracts WHERE article_id = new.id));
END;

CREATE TRIGGER article_extracts_ai AFTER INSERT ON article_extracts BEGIN
    INSERT INTO articles_fts(articles_fts, rowid, title, summary_text, content_text, author, extracted_text)
    SELECT 'delete', a.id, a.title, a.summary_text, a.content_text, a.author, NULL
    FROM articles a WHERE a.id = new.article_id;
    INSERT INTO articles_fts(rowid, title, summary_text, content_text, author, extracted_text)
    SELECT a.id, a.title, a.summary_text, a.content_text, a.author, new.text
    FROM articles a WHERE a.id = new.article_id;
END;
CREATE TRIGGER article_extracts_au AFTER UPDATE ON article_extracts BEGIN
    INSERT INTO articles_fts(articles_fts, rowid, title, summary_text, content_text, author, extracted_text)
    SELECT 'delete', a.id, a.title, a.summary_text, a.content_text, a.author, old.text
    FROM articles a WHERE a.id = old.article_id;
    INSERT INTO articles_fts(rowid, title, summary_text, content_text, author, extracted_text)
    SELECT a.id, a.title, a.summary_text, a.content_text, a.author, new.text
    FROM articles a WHERE a.id = new.article_id;
END;
CREATE TRIGGER article_extracts_ad AFTER DELETE ON article_extracts BEGIN
    INSERT INTO articles_fts(articles_fts, rowid, title, summary_text, content_text, author, extracted_text)
    SELECT 'delete', a.id, a.title, a.summary_text, a.content_text, a.author, old.text
    FROM articles a WHERE a.id = old.article_id;
    INSERT INTO articles_fts(rowid, title, summary_text, content_text, author, extracted_text)
    SELECT a.id, a.title, a.summary_text, a.content_text, a.author, NULL
    FROM articles a WHERE a.id = old.article_id;
END;

INSERT INTO articles_fts(articles_fts) VALUES ('rebuild');

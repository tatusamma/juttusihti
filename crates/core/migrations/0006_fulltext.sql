-- Full article text fetched from the article's web page, kept apart from the
-- feed's own content so a feed refresh never overwrites it. It is a cache:
-- rows may be evicted under a size limit (bookmarked articles excepted).
-- picture_path is relative to the data directory.
CREATE TABLE article_fulltext (
    article_id    INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE,
    text          TEXT NOT NULL,
    paywalled     INTEGER NOT NULL CHECK (paywalled IN (0, 1)),
    picture_path  TEXT,
    text_bytes    INTEGER NOT NULL,
    picture_bytes INTEGER NOT NULL DEFAULT 0,
    fetched_at    INTEGER NOT NULL
);
CREATE INDEX article_fulltext_fetched ON article_fulltext(fetched_at);

-- The search index now also covers the fetched text. Its external content is
-- a view joining both tables, so 'rebuild' and integrity checks see exactly
-- what the triggers index.
DROP TRIGGER articles_ai;
DROP TRIGGER articles_ad;
DROP TRIGGER articles_au;
DROP TABLE articles_fts;

CREATE VIEW articles_search AS
    SELECT a.id AS id, a.title AS title, a.summary_text AS summary_text, a.content_text AS content_text,
           a.author AS author, ft.text AS fetched_text
    FROM articles a LEFT JOIN article_fulltext ft ON ft.article_id = a.id;

-- remove_diacritics 0: Finnish ä/ö/å are distinct letters, not accented a/o.
CREATE VIRTUAL TABLE articles_fts USING fts5(
    title, summary_text, content_text, author, fetched_text,
    content = 'articles_search', content_rowid = 'id',
    tokenize = 'unicode61 remove_diacritics 0'
);

-- A new article cannot have fetched text yet.
CREATE TRIGGER articles_ai AFTER INSERT ON articles BEGIN
    INSERT INTO articles_fts(rowid, title, summary_text, content_text, author, fetched_text)
    VALUES (new.id, new.title, new.summary_text, new.content_text, new.author, NULL);
END;

-- BEFORE, not AFTER: the fetched text is still there to be named in the
-- 'delete', whereas ON DELETE CASCADE removes it once the article row is gone.
CREATE TRIGGER articles_bd BEFORE DELETE ON articles BEGIN
    INSERT INTO articles_fts(articles_fts, rowid, title, summary_text, content_text, author, fetched_text)
    VALUES ('delete', old.id, old.title, old.summary_text, old.content_text, old.author,
            (SELECT text FROM article_fulltext WHERE article_id = old.id));
END;

CREATE TRIGGER articles_au AFTER UPDATE OF title, summary_text, content_text, author ON articles BEGIN
    INSERT INTO articles_fts(articles_fts, rowid, title, summary_text, content_text, author, fetched_text)
    VALUES ('delete', old.id, old.title, old.summary_text, old.content_text, old.author,
            (SELECT text FROM article_fulltext WHERE article_id = old.id));
    INSERT INTO articles_fts(rowid, title, summary_text, content_text, author, fetched_text)
    VALUES (new.id, new.title, new.summary_text, new.content_text, new.author,
            (SELECT text FROM article_fulltext WHERE article_id = new.id));
END;

CREATE TRIGGER article_fulltext_ai AFTER INSERT ON article_fulltext BEGIN
    INSERT INTO articles_fts(articles_fts, rowid, title, summary_text, content_text, author, fetched_text)
    SELECT 'delete', a.id, a.title, a.summary_text, a.content_text, a.author, NULL
    FROM articles a WHERE a.id = new.article_id;
    INSERT INTO articles_fts(rowid, title, summary_text, content_text, author, fetched_text)
    SELECT a.id, a.title, a.summary_text, a.content_text, a.author, new.text
    FROM articles a WHERE a.id = new.article_id;
END;

CREATE TRIGGER article_fulltext_au AFTER UPDATE OF text ON article_fulltext BEGIN
    INSERT INTO articles_fts(articles_fts, rowid, title, summary_text, content_text, author, fetched_text)
    SELECT 'delete', a.id, a.title, a.summary_text, a.content_text, a.author, old.text
    FROM articles a WHERE a.id = old.article_id;
    INSERT INTO articles_fts(rowid, title, summary_text, content_text, author, fetched_text)
    SELECT a.id, a.title, a.summary_text, a.content_text, a.author, new.text
    FROM articles a WHERE a.id = new.article_id;
END;

-- During ON DELETE CASCADE the article row is already gone and articles_bd
-- has removed the whole entry, so the SELECTs match nothing. Only a direct
-- delete (eviction, clearing) re-indexes the article without fetched text.
CREATE TRIGGER article_fulltext_ad AFTER DELETE ON article_fulltext BEGIN
    INSERT INTO articles_fts(articles_fts, rowid, title, summary_text, content_text, author, fetched_text)
    SELECT 'delete', a.id, a.title, a.summary_text, a.content_text, a.author, old.text
    FROM articles a WHERE a.id = old.article_id;
    INSERT INTO articles_fts(rowid, title, summary_text, content_text, author, fetched_text)
    SELECT a.id, a.title, a.summary_text, a.content_text, a.author, NULL
    FROM articles a WHERE a.id = old.article_id;
END;

INSERT INTO articles_fts(articles_fts) VALUES ('rebuild');

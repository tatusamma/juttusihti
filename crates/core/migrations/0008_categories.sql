CREATE TABLE category_groups (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    normalized TEXT NOT NULL UNIQUE,
    position   INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
ALTER TABLE labels ADD COLUMN group_id INTEGER REFERENCES category_groups(id) ON DELETE SET NULL;
ALTER TABLE labels ADD COLUMN position INTEGER NOT NULL DEFAULT 0;
CREATE INDEX labels_group ON labels(group_id, position);

-- Publisher category words, per feed. Unsorted = label_id IS NULL AND ignored = 0.
CREATE TABLE feed_terms (
    id            INTEGER PRIMARY KEY,
    feed_id       INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    normalized    TEXT NOT NULL,
    scheme        TEXT NOT NULL DEFAULT '',
    label_id      INTEGER REFERENCES labels(id) ON DELETE SET NULL,
    ignored       INTEGER NOT NULL DEFAULT 0 CHECK (ignored IN (0, 1)),
    first_seen_at INTEGER NOT NULL,
    CHECK (ignored = 0 OR label_id IS NULL),
    UNIQUE (feed_id, normalized, scheme)
);
CREATE INDEX feed_terms_label ON feed_terms(label_id);

CREATE TABLE article_terms (
    article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    term_id    INTEGER NOT NULL REFERENCES feed_terms(id) ON DELETE CASCADE,
    PRIMARY KEY (article_id, term_id)
) WITHOUT ROWID;
CREATE INDEX article_terms_term ON article_terms(term_id, article_id);

CREATE TABLE favourites (
    id       INTEGER PRIMARY KEY,
    group_id INTEGER UNIQUE REFERENCES category_groups(id) ON DELETE CASCADE,
    label_id INTEGER UNIQUE REFERENCES labels(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    CHECK ((group_id IS NULL) <> (label_id IS NULL))
);

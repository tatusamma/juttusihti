-- Categories: their own entity, separate from labels. Groups hold categories;
-- publisher terms map to categories; an article may have a manual and an AI
-- category. `labels` and `article_labels` are not touched.
CREATE TABLE category_groups (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    normalized TEXT NOT NULL UNIQUE,
    position   INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE categories (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    normalized TEXT NOT NULL UNIQUE,
    group_id   INTEGER REFERENCES category_groups(id) ON DELETE SET NULL,
    position   INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX categories_group ON categories(group_id, position);

-- Publisher category words, per feed. Unsorted = category_id IS NULL AND ignored = 0.
CREATE TABLE feed_terms (
    id            INTEGER PRIMARY KEY,
    feed_id       INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
    name          TEXT NOT NULL,
    normalized    TEXT NOT NULL,
    scheme        TEXT NOT NULL DEFAULT '',
    category_id   INTEGER REFERENCES categories(id) ON DELETE SET NULL,
    ignored       INTEGER NOT NULL DEFAULT 0 CHECK (ignored IN (0, 1)),
    first_seen_at INTEGER NOT NULL,
    CHECK (ignored = 0 OR category_id IS NULL),
    UNIQUE (feed_id, normalized, scheme)
);
CREATE INDEX feed_terms_category ON feed_terms(category_id);

CREATE TABLE article_terms (
    article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    term_id    INTEGER NOT NULL REFERENCES feed_terms(id) ON DELETE CASCADE,
    PRIMARY KEY (article_id, term_id)
) WITHOUT ROWID;
CREATE INDEX article_terms_term ON article_terms(term_id, article_id);

-- An article's manual category (wins over everything) and the AI's main
-- category, kept apart so clearing the manual one returns to the AI one.
CREATE TABLE article_categories (
    article_id       INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE,
    user_category_id INTEGER REFERENCES categories(id) ON DELETE SET NULL,
    ai_category_id   INTEGER REFERENCES categories(id) ON DELETE SET NULL,
    updated_at       INTEGER NOT NULL
);
CREATE INDEX article_categories_user ON article_categories(user_category_id);
CREATE INDEX article_categories_ai ON article_categories(ai_category_id);

CREATE TABLE favourites (
    id          INTEGER PRIMARY KEY,
    group_id    INTEGER UNIQUE REFERENCES category_groups(id) ON DELETE CASCADE,
    category_id INTEGER UNIQUE REFERENCES categories(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    CHECK ((group_id IS NULL) <> (category_id IS NULL))
);

-- Articles the "find categories with AI" action has already sent, so the next
-- press offers the next ones (also those the AI found no category for).
CREATE TABLE category_ai_checks (
    article_id INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE,
    checked_at INTEGER NOT NULL
);

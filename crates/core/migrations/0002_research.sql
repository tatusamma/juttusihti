-- Milestone 2: research data model.
-- origin: 'user' | 'ai'. status: 'suggested' | 'confirmed' | 'rejected'.
-- AI rows always start as 'suggested'; only a user action confirms them.

CREATE TABLE analysis_sessions (
    id            INTEGER PRIMARY KEY,
    created_at    INTEGER NOT NULL,
    status        TEXT NOT NULL CHECK (status IN
                    ('draft', 'estimated', 'awaiting_approval', 'running', 'paused', 'cancelled', 'failed', 'completed')),
    config_json   TEXT NOT NULL,
    estimate_json TEXT,
    error         TEXT,
    completed_at  INTEGER
);

-- Stable snapshot of the articles a session covers.
CREATE TABLE analysis_session_articles (
    session_id INTEGER NOT NULL REFERENCES analysis_sessions(id) ON DELETE CASCADE,
    article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    PRIMARY KEY (session_id, article_id)
);

CREATE TABLE ai_suggestions (
    id             INTEGER PRIMARY KEY,
    session_id     INTEGER REFERENCES analysis_sessions(id) ON DELETE SET NULL,
    kind           TEXT NOT NULL,
    payload_json   TEXT NOT NULL,
    provider       TEXT NOT NULL,
    model          TEXT NOT NULL,
    prompt_version TEXT NOT NULL,
    confidence     REAL,
    evidence       TEXT,
    status         TEXT NOT NULL CHECK (status IN ('suggested', 'confirmed', 'rejected')),
    created_at     INTEGER NOT NULL,
    decided_at     INTEGER
);
CREATE INDEX ai_suggestions_session ON ai_suggestions(session_id, status);

CREATE TABLE labels (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    normalized TEXT NOT NULL UNIQUE,
    color      TEXT,
    created_at INTEGER NOT NULL
);

CREATE TABLE article_labels (
    article_id    INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    label_id      INTEGER NOT NULL REFERENCES labels(id) ON DELETE CASCADE,
    origin        TEXT NOT NULL CHECK (origin IN ('user', 'ai')),
    status        TEXT NOT NULL CHECK (status IN ('suggested', 'confirmed', 'rejected')),
    suggestion_id INTEGER REFERENCES ai_suggestions(id) ON DELETE SET NULL,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (article_id, label_id)
);
CREATE INDEX article_labels_label ON article_labels(label_id, status);

CREATE TABLE entities (
    id         INTEGER PRIMARY KEY,
    kind       TEXT NOT NULL CHECK (kind IN ('person', 'organization', 'place', 'concept', 'other')),
    name       TEXT NOT NULL,
    normalized TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (kind, normalized)
);

CREATE TABLE entity_aliases (
    entity_id  INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    alias      TEXT NOT NULL,
    normalized TEXT NOT NULL,
    PRIMARY KEY (entity_id, normalized)
);
CREATE INDEX entity_aliases_normalized ON entity_aliases(normalized);

CREATE TABLE article_entities (
    id            INTEGER PRIMARY KEY,
    article_id    INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    entity_id     INTEGER NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    role          TEXT NOT NULL DEFAULT 'mentioned',
    evidence      TEXT,
    origin        TEXT NOT NULL CHECK (origin IN ('user', 'ai')),
    status        TEXT NOT NULL CHECK (status IN ('suggested', 'confirmed', 'rejected')),
    suggestion_id INTEGER REFERENCES ai_suggestions(id) ON DELETE SET NULL,
    created_at    INTEGER NOT NULL,
    UNIQUE (article_id, entity_id, role)
);
CREATE INDEX article_entities_entity ON article_entities(entity_id, status);

CREATE TABLE events (
    id          INTEGER PRIMARY KEY,
    title       TEXT NOT NULL,
    description TEXT,
    -- Dates described by the content; NULL when unknown. Never inferred silently.
    starts_at   INTEGER,
    ends_at     INTEGER,
    origin      TEXT NOT NULL CHECK (origin IN ('user', 'ai')),
    status      TEXT NOT NULL CHECK (status IN ('suggested', 'confirmed', 'rejected')),
    created_at  INTEGER NOT NULL
);

CREATE TABLE article_events (
    article_id    INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    event_id      INTEGER NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    origin        TEXT NOT NULL CHECK (origin IN ('user', 'ai')),
    status        TEXT NOT NULL CHECK (status IN ('suggested', 'confirmed', 'rejected')),
    suggestion_id INTEGER REFERENCES ai_suggestions(id) ON DELETE SET NULL,
    created_at    INTEGER NOT NULL,
    PRIMARY KEY (article_id, event_id)
);
CREATE INDEX article_events_event ON article_events(event_id, status);

CREATE TABLE article_relationships (
    id            INTEGER PRIMARY KEY,
    source_id     INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    target_id     INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL CHECK (kind IN
                    ('duplicate', 'same_event', 'follow_up', 'related', 'citation', 'conflicting_claim')),
    origin        TEXT NOT NULL CHECK (origin IN ('user', 'ai')),
    status        TEXT NOT NULL CHECK (status IN ('suggested', 'confirmed', 'rejected')),
    evidence      TEXT,
    confidence    REAL CHECK (confidence IS NULL OR (confidence >= 0 AND confidence <= 1)),
    suggestion_id INTEGER REFERENCES ai_suggestions(id) ON DELETE SET NULL,
    created_at    INTEGER NOT NULL,
    CHECK (source_id <> target_id),
    UNIQUE (source_id, target_id, kind)
);
CREATE INDEX article_relationships_target ON article_relationships(target_id);

CREATE TABLE article_locations (
    id                INTEGER PRIMARY KEY,
    article_id        INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    place_name        TEXT NOT NULL,
    country           TEXT,
    latitude          REAL,
    longitude         REAL,
    role              TEXT NOT NULL CHECK (role IN ('event', 'entity_associated', 'publisher', 'mentioned')),
    evidence          TEXT,
    confidence        REAL,
    coordinate_source TEXT,
    ambiguous         INTEGER NOT NULL DEFAULT 0 CHECK (ambiguous IN (0, 1)),
    origin            TEXT NOT NULL CHECK (origin IN ('user', 'ai')),
    status            TEXT NOT NULL CHECK (status IN ('suggested', 'confirmed', 'rejected')),
    created_at        INTEGER NOT NULL,
    CHECK ((latitude IS NULL) = (longitude IS NULL))
);

CREATE TABLE annotations (
    id         INTEGER PRIMARY KEY,
    article_id INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    session_id INTEGER REFERENCES analysis_sessions(id) ON DELETE SET NULL,
    body       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

-- Cache key: content hash + provider + model + task + prompt version + settings hash.
CREATE TABLE ai_result_cache (
    cache_key   TEXT PRIMARY KEY,
    article_id  INTEGER REFERENCES articles(id) ON DELETE CASCADE,
    task        TEXT NOT NULL,
    result_json TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);

CREATE TABLE usage_ledger (
    id                  INTEGER PRIMARY KEY,
    session_id          INTEGER REFERENCES analysis_sessions(id) ON DELETE SET NULL,
    provider            TEXT NOT NULL,
    model               TEXT NOT NULL,
    operation           TEXT NOT NULL,
    input_tokens        INTEGER NOT NULL DEFAULT 0,
    cached_input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens       INTEGER NOT NULL DEFAULT 0,
    reasoning_tokens    INTEGER NOT NULL DEFAULT 0,
    -- NULL means unknown price, never zero.
    cost_microunits     INTEGER,
    currency            TEXT,
    created_at          INTEGER NOT NULL
);
CREATE INDEX usage_ledger_session ON usage_ledger(session_id);

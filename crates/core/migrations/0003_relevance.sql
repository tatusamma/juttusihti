-- Relevance classifications, kept separate from articles so filtering is
-- always reversible and never deletes anything.
CREATE TABLE article_relevance (
    id            INTEGER PRIMARY KEY,
    article_id    INTEGER NOT NULL REFERENCES articles(id) ON DELETE CASCADE,
    session_id    INTEGER REFERENCES analysis_sessions(id) ON DELETE CASCADE,
    class         TEXT NOT NULL CHECK (class IN
                    ('relevant', 'possibly_relevant', 'unrelated', 'duplicate', 'low_information', 'clickbait')),
    reason        TEXT,
    origin        TEXT NOT NULL CHECK (origin IN ('user', 'ai')),
    status        TEXT NOT NULL CHECK (status IN ('suggested', 'confirmed', 'rejected')),
    suggestion_id INTEGER REFERENCES ai_suggestions(id) ON DELETE SET NULL,
    created_at    INTEGER NOT NULL,
    UNIQUE (article_id, session_id)
);
CREATE INDEX article_relevance_session ON article_relevance(session_id, class, status);

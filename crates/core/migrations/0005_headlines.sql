-- AI-rewritten headlines for clickbait or vague titles. Kept apart from
-- articles: the original title is never modified, and the user can keep
-- or reject each rewrite.
CREATE TABLE article_headlines (
    article_id    INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE,
    headline      TEXT NOT NULL,
    status        TEXT NOT NULL CHECK (status IN ('suggested', 'confirmed', 'rejected')),
    session_id    INTEGER REFERENCES analysis_sessions(id) ON DELETE SET NULL,
    suggestion_id INTEGER REFERENCES ai_suggestions(id) ON DELETE SET NULL,
    created_at    INTEGER NOT NULL,
    decided_at    INTEGER
);

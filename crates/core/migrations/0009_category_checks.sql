-- Articles the "find categories with AI" action has already sent, so the next
-- press offers the next ones (also those the AI found no category for).
CREATE TABLE category_ai_checks (
    article_id INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE,
    checked_at INTEGER NOT NULL
);

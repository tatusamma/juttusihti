-- The latest fact check of an article. Only one is kept; checking again
-- replaces it. flagged_count (contradicted or misleading claims) is kept
-- beside the JSON so lists can flag articles without parsing it.
CREATE TABLE article_fact_checks (
    article_id     INTEGER PRIMARY KEY REFERENCES articles(id) ON DELETE CASCADE,
    checked_at     INTEGER NOT NULL,
    provider       TEXT NOT NULL,
    model          TEXT NOT NULL,
    prompt_version INTEGER NOT NULL,
    flagged_count  INTEGER NOT NULL,
    result_json    TEXT NOT NULL
);

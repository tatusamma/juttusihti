-- Indexes that match the list sort order (effective_at DESC, id DESC), so the
-- first page of any view is read straight from an index instead of sorting
-- every matching row (measured: Unread page 100 took 5 s at 1M articles).
DROP INDEX articles_effective_at;
DROP INDEX articles_feed_effective;
DROP INDEX articles_unread;
DROP INDEX articles_saved;
CREATE INDEX articles_effective_at ON articles(effective_at DESC, id DESC);
CREATE INDEX articles_feed_effective ON articles(feed_id, effective_at DESC, id DESC);
CREATE INDEX articles_unread_by_date ON articles(effective_at DESC, id DESC) WHERE is_read = 0;
CREATE INDEX articles_saved_by_date ON articles(effective_at DESC, id DESC) WHERE is_saved = 1;
-- Label / entity filters look up by article.
CREATE INDEX IF NOT EXISTS article_labels_article ON article_labels(article_id, label_id, status);
CREATE INDEX IF NOT EXISTS article_entities_article ON article_entities(article_id, entity_id, status);
-- Ingest's duplicate check (same feed, same canonical URL). Without it the
-- planner walked the whole feed through articles_feed_effective per insert.
CREATE INDEX articles_feed_dedup ON articles(feed_id, dedup_key);

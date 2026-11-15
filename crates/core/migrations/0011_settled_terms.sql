-- Whether a publisher term is settled: filed automatically, or decided by the user (moved to a
-- category or to Unsorted, ignored, filed by an approved AI draft). Only unsettled terms are
-- filed automatically, so the user's choices stay at later refreshes. Terms still in Unsorted
-- wait: they are filed when their category exists or once their name is on two articles.
ALTER TABLE feed_terms ADD COLUMN settled INTEGER NOT NULL DEFAULT 0 CHECK (settled IN (0, 1));
UPDATE feed_terms SET settled = 1 WHERE category_id IS NOT NULL OR ignored = 1;

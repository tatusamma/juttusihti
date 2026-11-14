-- Publisher categories are now filed automatically: each new term goes under the category
-- with the same name (see Store::ingest). This files the terms already waiting in Unsorted
-- the same way, once. Terms filed elsewhere or ignored, and names without letters or digits
-- (empty `normalized`), are left alone.

-- One new category per name that has none yet, in Ungrouped after the existing ones, most
-- used first. Its name is the spelling on the most articles.
WITH waiting AS (
    SELECT t.id, t.name, t.normalized,
           (SELECT COUNT(*) FROM article_terms a WHERE a.term_id = t.id) AS uses
    FROM feed_terms t
    WHERE t.category_id IS NULL AND t.ignored = 0 AND t.normalized <> ''
      AND NOT EXISTS (SELECT 1 FROM categories c WHERE c.normalized = t.normalized)
),
named AS (
    SELECT normalized,
           (SELECT trim(w2.name) FROM waiting w2 WHERE w2.normalized = w.normalized
            ORDER BY w2.uses DESC, w2.id LIMIT 1) AS name,
           SUM(uses) AS uses
    FROM waiting w
    GROUP BY normalized
)
INSERT INTO categories(name, normalized, group_id, position, created_at)
SELECT name, normalized, NULL,
       (SELECT COALESCE(MAX(position) + 1, 0) FROM categories WHERE group_id IS NULL)
           + ROW_NUMBER() OVER (ORDER BY uses DESC, name) - 1,
       CAST(strftime('%s', 'now') AS INTEGER)
FROM named;

UPDATE feed_terms
SET category_id = (SELECT c.id FROM categories c WHERE c.normalized = feed_terms.normalized)
WHERE category_id IS NULL AND ignored = 0 AND normalized <> '';

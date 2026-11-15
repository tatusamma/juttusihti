-- Publisher categories are now filed automatically (see Store::ingest). This files the terms
-- already waiting in Unsorted the same way, once: under the category with the same name when
-- it exists, otherwise in a new category, but only for a name on at least two articles (the
-- rest keep waiting, so one-off names do not each become a category). Terms filed elsewhere or
-- ignored, names without letters or digits (empty `normalized`), and the terms of a feed with a
-- default category (the user chose where its articles go) are left alone.

-- One new category per name that has none yet and is on two or more articles, in Ungrouped
-- after the existing ones, most used first. Its name is the spelling on the most articles.
WITH waiting AS (
    SELECT t.id, t.name, t.normalized,
           (SELECT COUNT(*) FROM article_terms a WHERE a.term_id = t.id) AS uses
    FROM feed_terms t
    WHERE t.category_id IS NULL AND t.ignored = 0 AND t.normalized <> ''
      AND t.feed_id NOT IN (SELECT id FROM feeds WHERE default_category_id IS NOT NULL)
      AND NOT EXISTS (SELECT 1 FROM categories c WHERE c.normalized = t.normalized)
),
named AS (
    SELECT w.normalized,
           (SELECT trim(w2.name) FROM waiting w2 WHERE w2.normalized = w.normalized
            ORDER BY w2.uses DESC, w2.id LIMIT 1) AS name,
           SUM(w.uses) AS uses,
           (SELECT COUNT(DISTINCT a.article_id) FROM article_terms a
            JOIN waiting w3 ON w3.id = a.term_id WHERE w3.normalized = w.normalized) AS articles
    FROM waiting w
    GROUP BY w.normalized
)
INSERT INTO categories(name, normalized, group_id, position, created_at)
SELECT name, normalized, NULL,
       (SELECT COALESCE(MAX(position) + 1, 0) FROM categories WHERE group_id IS NULL)
           + ROW_NUMBER() OVER (ORDER BY uses DESC, name) - 1,
       CAST(strftime('%s', 'now') AS INTEGER)
FROM named
WHERE articles >= 2;

UPDATE feed_terms
SET category_id = (SELECT c.id FROM categories c WHERE c.normalized = feed_terms.normalized)
WHERE category_id IS NULL AND ignored = 0 AND normalized <> ''
  AND feed_id NOT IN (SELECT id FROM feeds WHERE default_category_id IS NOT NULL)
  AND EXISTS (SELECT 1 FROM categories c WHERE c.normalized = feed_terms.normalized);

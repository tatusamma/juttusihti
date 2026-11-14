-- A feed's default category (for feeds that send no categories) and the
-- category group it is filed under. Both optional; nothing is rewritten.
ALTER TABLE feeds ADD COLUMN default_category_id INTEGER REFERENCES categories(id) ON DELETE SET NULL;
ALTER TABLE feeds ADD COLUMN category_group_id INTEGER REFERENCES category_groups(id) ON DELETE SET NULL;
CREATE INDEX feeds_category_group ON feeds(category_group_id);

-- A hidden category's articles leave the article list, except when the category itself is viewed.
ALTER TABLE categories ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;

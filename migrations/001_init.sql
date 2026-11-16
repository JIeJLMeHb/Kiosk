CREATE TABLE event (
  id          INTEGER PRIMARY KEY,
  name        TEXT NOT NULL UNIQUE,
  starts_on   TEXT,
  ends_on     TEXT,
  description TEXT NOT NULL DEFAULT ''
);

CREATE TABLE photo (
  id          INTEGER PRIMARY KEY,
  hash        TEXT NOT NULL UNIQUE,
  rel_path    TEXT NOT NULL,
  bytes       INTEGER NOT NULL,
  mime        TEXT NOT NULL,
  width       INTEGER NOT NULL,
  height      INTEGER NOT NULL,
  taken_at    TEXT,                                   -- 'YYYY-MM-DDTHH:MM:SS' (локальное)
  taken_day   TEXT GENERATED ALWAYS AS (substr(taken_at,1,10)) STORED,
  taken_src   TEXT NOT NULL DEFAULT 'unknown',        -- exif|mtime|manual|unknown
  imported_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
  title       TEXT NOT NULL DEFAULT '',
  note        TEXT NOT NULL DEFAULT '',
  search_text TEXT NOT NULL DEFAULT '',               -- нижний регистр, для LIKE и FTS
  event_id    INTEGER REFERENCES event(id) ON DELETE SET NULL,
  rating      INTEGER NOT NULL DEFAULT 0,
  hidden      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX photo_day   ON photo(taken_day);
CREATE INDEX photo_event ON photo(event_id);

CREATE TABLE tag (
  id        INTEGER PRIMARY KEY,
  name      TEXT NOT NULL,
  kind      TEXT NOT NULL DEFAULT 'custom',           -- person|place|custom
  parent_id INTEGER REFERENCES tag(id) ON DELETE CASCADE,
  UNIQUE(name, kind)
);
CREATE TABLE photo_tag (
  photo_id INTEGER NOT NULL REFERENCES photo(id) ON DELETE CASCADE,
  tag_id   INTEGER NOT NULL REFERENCES tag(id)   ON DELETE CASCADE,
  PRIMARY KEY (photo_id, tag_id)
);
CREATE INDEX photo_tag_rev ON photo_tag(tag_id, photo_id);

CREATE TABLE saved_search (
  id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, query_json TEXT NOT NULL
);
CREATE TABLE settings (key TEXT PRIMARY KEY, value_json TEXT NOT NULL);

-- trigram: подстрочный поиск по-русски без морфологии, запрос от 3 символов
CREATE VIRTUAL TABLE photo_fts USING fts5(body, tokenize='trigram');

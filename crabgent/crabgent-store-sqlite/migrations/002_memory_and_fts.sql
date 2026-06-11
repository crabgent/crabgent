-- Initial design8: long-term memory storage + FTS5 search across memory and
-- sessions.
--
-- Conventions follow 001_initial_schema.sql: TEXT ids, TEXT timestamps,
-- INTEGER booleans. Optional scope fields are NULL when unset.

-- ===== memory documents =====

CREATE TABLE IF NOT EXISTS memory (
    id          TEXT    PRIMARY KEY,
    owner       TEXT,
    channel     TEXT,
    conv        TEXT,
    agent       TEXT,
    kind        TEXT,
    body        TEXT    NOT NULL,
    created_at  TEXT    NOT NULL,
    updated_at  TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS memory_owner_created
    ON memory (owner, created_at DESC);

CREATE INDEX IF NOT EXISTS memory_scope
    ON memory (owner, channel, conv, agent, kind);

-- FTS5 index over memory.body. content='memory' makes the FTS index a
-- contentless mirror of the memory table; triggers keep them in sync.
CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
    body,
    content='memory',
    content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS memory_after_insert AFTER INSERT ON memory
BEGIN
    INSERT INTO memory_fts(rowid, body) VALUES (new.rowid, new.body);
END;

CREATE TRIGGER IF NOT EXISTS memory_after_delete AFTER DELETE ON memory
BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, body)
        VALUES ('delete', old.rowid, old.body);
END;

CREATE TRIGGER IF NOT EXISTS memory_after_update AFTER UPDATE ON memory
BEGIN
    INSERT INTO memory_fts(memory_fts, rowid, body)
        VALUES ('delete', old.rowid, old.body);
    INSERT INTO memory_fts(rowid, body) VALUES (new.rowid, new.body);
END;

-- ===== session-search index =====
--
-- session_search holds a flattened text aggregation of each session's
-- messages so FTS5 can search across them. The application layer
-- populates this whenever a session is saved.

CREATE TABLE IF NOT EXISTS session_search (
    session_id  TEXT    PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    owner       TEXT    NOT NULL,
    body        TEXT    NOT NULL DEFAULT '',
    updated_at  TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS session_search_owner
    ON session_search (owner, updated_at DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS session_messages_fts USING fts5(
    body,
    content='session_search',
    content_rowid='rowid'
);

CREATE TRIGGER IF NOT EXISTS session_search_after_insert AFTER INSERT ON session_search
BEGIN
    INSERT INTO session_messages_fts(rowid, body) VALUES (new.rowid, new.body);
END;

CREATE TRIGGER IF NOT EXISTS session_search_after_delete AFTER DELETE ON session_search
BEGIN
    INSERT INTO session_messages_fts(session_messages_fts, rowid, body)
        VALUES ('delete', old.rowid, old.body);
END;

CREATE TRIGGER IF NOT EXISTS session_search_after_update AFTER UPDATE ON session_search
BEGIN
    INSERT INTO session_messages_fts(session_messages_fts, rowid, body)
        VALUES ('delete', old.rowid, old.body);
    INSERT INTO session_messages_fts(rowid, body) VALUES (new.rowid, new.body);
END;

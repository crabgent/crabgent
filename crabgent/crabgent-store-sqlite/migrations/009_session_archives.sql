CREATE TABLE IF NOT EXISTS session_archives (
    id         TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    messages   TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_session_archives_session_id
    ON session_archives(session_id);

CREATE INDEX IF NOT EXISTS idx_session_archives_created_at
    ON session_archives(created_at);

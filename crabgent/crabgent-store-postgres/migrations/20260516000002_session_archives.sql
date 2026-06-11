CREATE TABLE IF NOT EXISTS session_archives (
    id         UUID PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    messages   JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_session_archives_session_id
    ON session_archives(session_id);

CREATE INDEX IF NOT EXISTS idx_session_archives_created_at
    ON session_archives(created_at);

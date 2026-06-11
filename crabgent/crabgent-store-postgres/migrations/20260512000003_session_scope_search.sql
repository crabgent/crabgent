-- Extend sessions with memory-compatible search scope and generated FTS.

ALTER TABLE sessions ADD COLUMN IF NOT EXISTS scope_channel TEXT;
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS scope_conv TEXT;
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS scope_agent TEXT;
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS scope_kind TEXT;
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS search_body TEXT NOT NULL DEFAULT '';
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS search_vector TSVECTOR
    GENERATED ALWAYS AS (
        to_tsvector(
            'english'::regconfig,
            coalesce(title, '') || ' ' || coalesce(summary, '') || ' ' || search_body
        )
    ) STORED;

CREATE INDEX IF NOT EXISTS sessions_scope_search
    ON sessions (owner, scope_channel, scope_conv, scope_agent, scope_kind, updated_at DESC);

CREATE INDEX IF NOT EXISTS sessions_search_vector_gin
    ON sessions USING GIN (search_vector);

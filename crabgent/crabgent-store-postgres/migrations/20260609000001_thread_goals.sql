-- Per-session thread goal: an explicit objective the model keeps working
-- toward across turns. Singleton per session (one goal row per session_id);
-- the UNIQUE constraint on session_id enforces that invariant atomically, so a
-- concurrent second create surfaces as SQLSTATE 23505 (mapped to a conflict).
CREATE TABLE IF NOT EXISTS thread_goals (
    id                UUID        PRIMARY KEY,
    owner             TEXT        NOT NULL,
    session_id        UUID        NOT NULL UNIQUE,
    objective         TEXT        NOT NULL,
    status            TEXT        NOT NULL DEFAULT 'active',
    token_budget      BIGINT,
    tokens_used       BIGINT      NOT NULL DEFAULT 0,
    time_used_seconds BIGINT      NOT NULL DEFAULT 0,
    created_at        TIMESTAMPTZ NOT NULL,
    updated_at        TIMESTAMPTZ NOT NULL
);

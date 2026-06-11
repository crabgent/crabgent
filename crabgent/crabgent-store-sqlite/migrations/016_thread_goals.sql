-- Per-session thread goal: an explicit objective the model keeps working
-- toward across turns. Singleton per session (one goal row per session_id);
-- the UNIQUE constraint on session_id enforces that invariant atomically, so
-- a concurrent second create surfaces as a unique-constraint conflict.
CREATE TABLE thread_goals (
    id                TEXT    PRIMARY KEY,
    owner             TEXT    NOT NULL,
    session_id        TEXT    NOT NULL UNIQUE,
    objective         TEXT    NOT NULL,
    status            TEXT    NOT NULL DEFAULT 'active',
    token_budget      INTEGER,
    tokens_used       INTEGER NOT NULL DEFAULT 0,
    time_used_seconds INTEGER NOT NULL DEFAULT 0,
    created_at        TEXT    NOT NULL,
    updated_at        TEXT    NOT NULL
);

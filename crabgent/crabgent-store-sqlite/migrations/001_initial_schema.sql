-- crabgent-store-sqlite initial schema.
--
-- Conventions:
--   * All ids are stored as TEXT (UUIDv7 canonical form). Time-ordered
--     uuids let lexicographic sort match wall-clock order.
--   * Timestamps are stored as TEXT (ISO 8601 / RFC3339) via
--     sqlx::types::chrono.
--   * Booleans are stored as INTEGER 0/1.
--   * JSON columns (messages, schedule, delivery_ctx) are stored as TEXT;
--     the application layer (de)serialises via serde_json.

CREATE TABLE IF NOT EXISTS sessions (
    id              TEXT    PRIMARY KEY,
    owner           TEXT    NOT NULL,
    thread          TEXT,
    title           TEXT,
    summary         TEXT,
    model_override  TEXT,
    messages        TEXT    NOT NULL DEFAULT '[]',
    created_at      TEXT    NOT NULL,
    updated_at      TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS sessions_owner_thread_updated
    ON sessions (owner, thread, updated_at DESC);

CREATE TABLE IF NOT EXISTS tasks (
    id                  TEXT    PRIMARY KEY,
    owner               TEXT    NOT NULL,
    prompt              TEXT    NOT NULL,
    status              TEXT    NOT NULL,
    output              TEXT    NOT NULL DEFAULT '',
    error               TEXT,
    created_at          TEXT    NOT NULL,
    updated_at          TEXT    NOT NULL,
    finished_at         TEXT,
    parent_session_id   TEXT,
    parent_task_id      TEXT,
    context_mode        TEXT
);

CREATE INDEX IF NOT EXISTS tasks_status_created
    ON tasks (status, created_at);

CREATE INDEX IF NOT EXISTS tasks_finished_at
    ON tasks (finished_at);

CREATE TABLE IF NOT EXISTS cron_jobs (
    id              TEXT    PRIMARY KEY,
    name            TEXT    NOT NULL,
    owner           TEXT,
    prompt          TEXT    NOT NULL,
    schedule        TEXT    NOT NULL,
    enabled         INTEGER NOT NULL DEFAULT 1,
    run_once        INTEGER NOT NULL DEFAULT 0,
    model_override  TEXT,
    pre_command     TEXT,
    delivery_ctx    TEXT    NOT NULL DEFAULT '{}',
    last_run        TEXT,
    next_run        TEXT    NOT NULL,
    created_at      TEXT    NOT NULL,
    claimed_at      TEXT
);

CREATE INDEX IF NOT EXISTS cron_jobs_due
    ON cron_jobs (enabled, claimed_at, next_run);

CREATE INDEX IF NOT EXISTS cron_jobs_owner_created
    ON cron_jobs (owner, created_at);

CREATE TABLE IF NOT EXISTS tool_cache (
    id          TEXT    NOT NULL,
    session_id  TEXT    NOT NULL,
    tool_name   TEXT    NOT NULL,
    content     TEXT    NOT NULL,
    preview     TEXT    NOT NULL,
    created_at  TEXT    NOT NULL,
    expires_at  TEXT    NOT NULL,
    PRIMARY KEY (id, session_id)
);

CREATE INDEX IF NOT EXISTS tool_cache_expires
    ON tool_cache (expires_at);

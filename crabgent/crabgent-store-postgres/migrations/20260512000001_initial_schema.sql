-- crabgent-store-postgres initial schema.
--
-- Conventions:
--   * Identifiers are native UUID values supplied by the application.
--   * Timestamps are native TIMESTAMPTZ.
--   * Structured payloads are JSONB.
--   * Booleans are native BOOLEAN.

CREATE TABLE IF NOT EXISTS sessions (
    id              UUID        PRIMARY KEY,
    owner           TEXT        NOT NULL,
    thread          TEXT,
    title           TEXT,
    summary         TEXT,
    model_override  TEXT,
    messages        JSONB       NOT NULL DEFAULT '[]'::jsonb,
    created_at      TIMESTAMPTZ NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS sessions_owner_thread_updated
    ON sessions (owner, thread, updated_at DESC);

CREATE TABLE IF NOT EXISTS tasks (
    id                  UUID        PRIMARY KEY,
    owner               TEXT        NOT NULL,
    prompt              TEXT        NOT NULL,
    status              TEXT        NOT NULL,
    output              TEXT        NOT NULL DEFAULT '',
    error               TEXT,
    created_at          TIMESTAMPTZ NOT NULL,
    updated_at          TIMESTAMPTZ NOT NULL,
    finished_at         TIMESTAMPTZ,
    parent_session_id   UUID,
    parent_task_id      UUID,
    context_mode        TEXT
);

CREATE INDEX IF NOT EXISTS tasks_status_created
    ON tasks (status, created_at);

CREATE INDEX IF NOT EXISTS tasks_finished_at
    ON tasks (finished_at);

CREATE TABLE IF NOT EXISTS cron_jobs (
    id              UUID        PRIMARY KEY,
    name            TEXT        NOT NULL,
    owner           TEXT,
    prompt          TEXT        NOT NULL,
    schedule        JSONB       NOT NULL,
    enabled         BOOLEAN     NOT NULL DEFAULT TRUE,
    run_once        BOOLEAN     NOT NULL DEFAULT FALSE,
    pre_command     TEXT,
    delivery_ctx    JSONB       NOT NULL DEFAULT '{}'::jsonb,
    last_run        TIMESTAMPTZ,
    next_run        TIMESTAMPTZ NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL,
    claimed_at      TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS cron_jobs_due
    ON cron_jobs (enabled, claimed_at, next_run);

CREATE INDEX IF NOT EXISTS cron_jobs_owner_created
    ON cron_jobs (owner, created_at);

CREATE TABLE IF NOT EXISTS tool_cache (
    id          TEXT        NOT NULL,
    session_id  UUID        NOT NULL,
    tool_name   TEXT        NOT NULL,
    content     TEXT        NOT NULL,
    preview     TEXT        NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (id, session_id)
);

CREATE INDEX IF NOT EXISTS tool_cache_expires
    ON tool_cache (expires_at);

ALTER TABLE sessions
    ADD COLUMN IF NOT EXISTS reasoning_effort_override TEXT
    CHECK (
        reasoning_effort_override IS NULL
        OR reasoning_effort_override IN ('low', 'medium', 'high')
    );

ALTER TABLE tasks
    ADD COLUMN IF NOT EXISTS reasoning_effort_override TEXT
    CHECK (
        reasoning_effort_override IS NULL
        OR reasoning_effort_override IN ('low', 'medium', 'high')
    );

ALTER TABLE cron_jobs
    ADD COLUMN IF NOT EXISTS reasoning_effort_override TEXT
    CHECK (
        reasoning_effort_override IS NULL
        OR reasoning_effort_override IN ('low', 'medium', 'high')
    );

CREATE TABLE IF NOT EXISTS global_reasoning_effort_overrides (
    singleton        SMALLINT PRIMARY KEY CHECK (singleton = 0),
    reasoning_effort TEXT NOT NULL CHECK (reasoning_effort IN ('low', 'medium', 'high'))
);

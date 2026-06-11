ALTER TABLE tasks
    ADD COLUMN IF NOT EXISTS transcript JSONB;

ALTER TABLE tasks
    ADD COLUMN IF NOT EXISTS resume_spec JSONB;

ALTER TABLE tasks
    ADD COLUMN IF NOT EXISTS resume_count BIGINT NOT NULL DEFAULT 0;

ALTER TABLE tasks
    ADD COLUMN IF NOT EXISTS pause_cause TEXT
    CHECK (
        pause_cause IS NULL
        OR pause_cause IN ('shutdown', 'forced', 'crash')
    );

ALTER TABLE tasks
    ADD COLUMN IF NOT EXISTS paused_at TIMESTAMPTZ;

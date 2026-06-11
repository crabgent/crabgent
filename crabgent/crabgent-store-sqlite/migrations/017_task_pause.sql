ALTER TABLE tasks
    ADD COLUMN transcript TEXT;

ALTER TABLE tasks
    ADD COLUMN resume_spec TEXT;

ALTER TABLE tasks
    ADD COLUMN resume_count INTEGER NOT NULL DEFAULT 0;

ALTER TABLE tasks
    ADD COLUMN pause_cause TEXT
    CHECK (
        pause_cause IS NULL
        OR pause_cause IN ('shutdown', 'forced', 'crash')
    );

ALTER TABLE tasks
    ADD COLUMN paused_at TEXT;

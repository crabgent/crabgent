-- Add full MemoryScope fields for cron jobs.

ALTER TABLE cron_jobs RENAME COLUMN owner TO scope_owner;

ALTER TABLE cron_jobs ADD COLUMN scope_channel TEXT NULL;
ALTER TABLE cron_jobs ADD COLUMN scope_conv TEXT NULL;
ALTER TABLE cron_jobs ADD COLUMN scope_agent TEXT NULL;
ALTER TABLE cron_jobs ADD COLUMN scope_kind TEXT NULL;

CREATE INDEX IF NOT EXISTS idx_cron_jobs_scope_kind_channel
    ON cron_jobs (scope_kind, scope_channel);

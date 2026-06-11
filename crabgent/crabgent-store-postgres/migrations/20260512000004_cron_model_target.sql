-- Add provider-qualified model target storage for cron jobs.

ALTER TABLE cron_jobs ADD COLUMN IF NOT EXISTS model_target JSONB;

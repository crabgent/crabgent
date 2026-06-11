ALTER TABLE memory ADD COLUMN class TEXT;
ALTER TABLE memory ADD COLUMN importance REAL;
ALTER TABLE memory ADD COLUMN expires_at TEXT;
ALTER TABLE memory ADD COLUMN archived_at TEXT;

CREATE INDEX IF NOT EXISTS memory_class
    ON memory (owner, class);

CREATE INDEX IF NOT EXISTS memory_expires
    ON memory (expires_at)
    WHERE expires_at IS NOT NULL;

CREATE INDEX IF NOT EXISTS memory_archived
    ON memory (archived_at)
    WHERE archived_at IS NOT NULL;

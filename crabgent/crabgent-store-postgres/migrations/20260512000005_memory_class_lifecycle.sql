ALTER TABLE memory_docs
    ADD COLUMN class TEXT,
    ADD COLUMN importance REAL,
    ADD COLUMN expires_at TIMESTAMPTZ,
    ADD COLUMN archived_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS memory_class
    ON memory_docs (owner, class);

CREATE INDEX IF NOT EXISTS memory_expires
    ON memory_docs (expires_at)
    WHERE expires_at IS NOT NULL;

CREATE INDEX IF NOT EXISTS memory_archived
    ON memory_docs (archived_at)
    WHERE archived_at IS NOT NULL;

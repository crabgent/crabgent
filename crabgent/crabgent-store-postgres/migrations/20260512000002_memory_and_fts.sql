-- Long-term memory storage with native Postgres full-text search.

CREATE TABLE IF NOT EXISTS memory_docs (
    id             UUID        PRIMARY KEY,
    owner          TEXT,
    channel        TEXT,
    conv           TEXT,
    agent          TEXT,
    kind           TEXT,
    body           TEXT        NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL,
    updated_at     TIMESTAMPTZ NOT NULL,
    search_vector  TSVECTOR    GENERATED ALWAYS AS (
        to_tsvector('english'::regconfig, body)
    ) STORED
);

CREATE INDEX IF NOT EXISTS memory_docs_owner_created
    ON memory_docs (owner, created_at DESC);

CREATE INDEX IF NOT EXISTS memory_docs_scope
    ON memory_docs (owner, channel, conv, agent, kind);

CREATE INDEX IF NOT EXISTS memory_docs_search_vector_gin
    ON memory_docs USING GIN (search_vector);

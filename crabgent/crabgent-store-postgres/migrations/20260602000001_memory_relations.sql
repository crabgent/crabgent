-- Directed edges between memory_docs nodes for the relation graph.
--
-- No SQL FOREIGN KEY to memory_docs: cascade-on-delete is enforced in
-- application code (PostgresMemoryStore::delete / delete_scoped) to stay
-- uniform with the sqlite and in-memory backends. The natural key uses
-- NULLS NOT DISTINCT so NULL-owner edges still deduplicate under Postgres
-- null-equality semantics, matching the in-memory natural_key_eq.

CREATE TABLE IF NOT EXISTS memory_relations (
    id             UUID        PRIMARY KEY,
    from_id        UUID        NOT NULL,
    to_id          UUID        NOT NULL,
    relation_type  TEXT        NOT NULL,
    owner          TEXT,
    channel        TEXT,
    conv           TEXT,
    agent          TEXT,
    kind           TEXT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE NULLS NOT DISTINCT (from_id, to_id, relation_type, owner)
);

CREATE INDEX IF NOT EXISTS memory_relations_from
    ON memory_relations (from_id);

CREATE INDEX IF NOT EXISTS memory_relations_to
    ON memory_relations (to_id);

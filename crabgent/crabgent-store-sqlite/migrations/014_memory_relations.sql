-- Memory relation edges between two memory documents.
-- No SQL FOREIGN KEY: the pool does not enable PRAGMA foreign_keys, so cascade
-- on document delete is performed in application code (see memory/relations.rs).
CREATE TABLE memory_relations (
    id TEXT PRIMARY KEY,
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    relation_type TEXT NOT NULL,
    owner TEXT,
    channel TEXT,
    conv TEXT,
    agent TEXT,
    kind TEXT,
    created_at TEXT NOT NULL,
    UNIQUE (from_id, to_id, relation_type, owner)
);

CREATE INDEX idx_memory_relations_from_id ON memory_relations (from_id);
CREATE INDEX idx_memory_relations_to_id ON memory_relations (to_id);

-- Dedup global-scope relation edges (owner IS NULL) on SQLite.
--
-- The table-level UNIQUE (from_id, to_id, relation_type, owner) from migration
-- 014 does not dedup global edges: SQLite treats NULLs as distinct in a UNIQUE
-- index, so two NULL-owner edges with the same (from_id, to_id, relation_type)
-- both insert. Postgres dedups these via NULLS NOT DISTINCT and the in-memory
-- backend via Rust equality. A partial unique index restores parity by making
-- the natural key unique when owner IS NULL.
CREATE UNIQUE INDEX IF NOT EXISTS idx_memory_relations_global_unique
    ON memory_relations (from_id, to_id, relation_type)
    WHERE owner IS NULL;

-- Close the concurrent `find_or_create` race for sessions.
--
-- Two concurrent kernel runs with the same `(owner, thread, scope.*)` tuple
-- previously both missed the SELECT-then-INSERT lookup and inserted distinct
-- rows. The query layer then surfaced two sessions for the same logical
-- conversation.
--
-- A unique index on the full scope tuple gives `INSERT ... ON CONFLICT DO
-- NOTHING RETURNING` an atomic decision boundary even when some scope columns
-- are NULL. PG 15+ NULLS NOT DISTINCT treats NULL as equal for the conflict
-- check, matching the application-level lookup semantics (the SELECT uses
-- `($N IS NULL AND col IS NULL) OR col = $N`).
--
-- The existing read-path index `sessions_scope_search` stays as the lookup
-- accelerator. This unique index is a write-path constraint.
CREATE UNIQUE INDEX IF NOT EXISTS sessions_scope_distinct
    ON sessions (owner, thread, scope_channel, scope_conv, scope_agent, scope_kind)
    NULLS NOT DISTINCT;

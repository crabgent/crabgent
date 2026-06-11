-- Extend session search with the same scope vector used by memory search.
-- Existing rows are left owner-scoped; future saves populate the narrower
-- optional dimensions when a Session carries them.

ALTER TABLE sessions ADD COLUMN scope_channel TEXT;
ALTER TABLE sessions ADD COLUMN scope_conv TEXT;
ALTER TABLE sessions ADD COLUMN scope_agent TEXT;
ALTER TABLE sessions ADD COLUMN scope_kind TEXT;

ALTER TABLE session_search ADD COLUMN channel TEXT;
ALTER TABLE session_search ADD COLUMN conv TEXT;
ALTER TABLE session_search ADD COLUMN agent TEXT;
ALTER TABLE session_search ADD COLUMN kind TEXT;

CREATE INDEX IF NOT EXISTS session_search_scope
    ON session_search (owner, channel, conv, agent, kind, updated_at DESC);

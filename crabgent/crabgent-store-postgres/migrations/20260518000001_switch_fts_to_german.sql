-- Switch full-text search to the german tokenizer.
--
-- GENERATED tsvector columns cannot read default_text_search_config (the
-- expression must be IMMUTABLE), so the language stays hardcoded in the
-- GENERATED clause. The Rust query side uses the 1-arg form of
-- websearch_to_tsquery/ts_headline that picks up default_text_search_config,
-- and ALTER DATABASE keeps that default in sync so query stemmer and index
-- stemmer agree.
--
-- Operators that want a different language run their own follow-up
-- migration that DROPs and re-creates the GENERATED columns and issues another
-- ALTER DATABASE.

-- ALTER DATABASE updates the default for future connections; SET also fixes
-- the current session so the migrating connection itself (and any sibling
-- connections sqlx reuses from the pool after this migration commits) match
-- the language baked into the GENERATED columns below.
DO $$
BEGIN
    EXECUTE format(
        'ALTER DATABASE %I SET default_text_search_config = ''german''',
        current_database()
    );
END
$$;
SET default_text_search_config = 'german';

DROP INDEX IF EXISTS memory_docs_search_vector_gin;
ALTER TABLE memory_docs DROP COLUMN IF EXISTS search_vector;
ALTER TABLE memory_docs
    ADD COLUMN search_vector TSVECTOR
    GENERATED ALWAYS AS (to_tsvector('german'::regconfig, body)) STORED;
CREATE INDEX memory_docs_search_vector_gin
    ON memory_docs USING GIN (search_vector);

DROP INDEX IF EXISTS sessions_search_vector_gin;
ALTER TABLE sessions DROP COLUMN IF EXISTS search_vector;
ALTER TABLE sessions
    ADD COLUMN search_vector TSVECTOR
    GENERATED ALWAYS AS (
        to_tsvector(
            'german'::regconfig,
            coalesce(title, '') || ' ' || coalesce(summary, '') || ' ' || search_body
        )
    ) STORED;
CREATE INDEX sessions_search_vector_gin
    ON sessions USING GIN (search_vector);

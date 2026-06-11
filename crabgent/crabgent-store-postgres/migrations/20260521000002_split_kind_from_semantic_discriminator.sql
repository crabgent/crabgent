-- Data-only migration. Reclassifies legacy `semantic` rows whose body starts
-- with a well-known Markdown prefix into the matching first-class MemoryClass
-- variant added in Option B of the memory-class upstream fix.
--
-- The `kind` column on `memory_docs` is the channel-kind (direct / group / im)
-- and is not touched. Rows with a non-`semantic` class are left alone even if
-- the body prefix happens to match (intentional: an `episodic` memory about a
-- skill stays episodic).
--
-- Body-prefix convention:
--   * The header prefix starts at byte 0 of `body`.
--   * The header is either the whole body or is followed by a newline.
--   * Mid-document Markdown headings are not semantic discriminators.

UPDATE memory_docs
SET class = 'user_profile'
WHERE class = 'semantic'
  AND (body = '# user:' OR body LIKE '# user:%' || chr(10) || '%');

UPDATE memory_docs
SET class = 'notes'
WHERE class = 'semantic'
  AND (body = '# notes' OR body LIKE '# notes' || chr(10) || '%');

UPDATE memory_docs
SET class = 'skill'
WHERE class = 'semantic'
  AND (body = '# skill:' OR body LIKE '# skill:%' || chr(10) || '%');

UPDATE memory_docs
SET class = 'tools'
WHERE class = 'semantic'
  AND (body = '# tools' OR body LIKE '# tools' || chr(10) || '%');

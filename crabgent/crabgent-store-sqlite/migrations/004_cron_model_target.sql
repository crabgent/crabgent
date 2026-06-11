-- Normalize cron_jobs.model_override from legacy raw strings into JSON-backed
-- ModelTargetDto values. Plain ids remain JSON strings. provider/model values
-- become objects so provider-qualified cron runs survive persistence.

UPDATE cron_jobs
SET model_override = CASE
    WHEN json_valid(model_override) THEN model_override
    WHEN instr(model_override, '/') > 1
         AND instr(model_override, '/') < length(model_override)
    THEN json_object(
        'provider',
        substr(model_override, 1, instr(model_override, '/') - 1),
        'id',
        substr(model_override, instr(model_override, '/') + 1)
    )
    ELSE json_quote(model_override)
END
WHERE model_override IS NOT NULL;

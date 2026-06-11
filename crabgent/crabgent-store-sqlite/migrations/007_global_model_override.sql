CREATE TABLE IF NOT EXISTS global_model_overrides (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 0),
    model_id  TEXT NOT NULL
);

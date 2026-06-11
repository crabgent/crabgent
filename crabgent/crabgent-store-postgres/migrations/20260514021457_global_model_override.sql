CREATE TABLE IF NOT EXISTS global_model_overrides (
    singleton SMALLINT PRIMARY KEY CHECK (singleton = 0),
    model_id  TEXT NOT NULL
);

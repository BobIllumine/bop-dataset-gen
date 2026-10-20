-- Portable across SQLite and PostgreSQL: TEXT and BIGINT only, times in Unix milliseconds.
CREATE TABLE bop_jobs (
    id TEXT PRIMARY KEY,
    state TEXT NOT NULL,
    renderer TEXT NOT NULL,
    export_bop BIGINT NOT NULL,
    config_toml TEXT NOT NULL,
    total_scenes BIGINT NOT NULL,
    committed_scenes BIGINT NOT NULL,
    error TEXT,
    created_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    started_at_ms BIGINT,
    finished_at_ms BIGINT
);

CREATE INDEX bop_jobs_state_created ON bop_jobs (state, created_at_ms);

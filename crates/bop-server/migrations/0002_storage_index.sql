-- An index of what storage holds, so listings and checks need no S3 round trip.
-- Storage stays the source of truth: rows are written after the object, removed
-- before it, and rebuilt from a listing when missing.

-- Result files uploaded below `datasets/<job_id>/`.
CREATE TABLE bop_job_files (
    job_id TEXT NOT NULL,
    path TEXT NOT NULL,
    size BIGINT NOT NULL,
    PRIMARY KEY (job_id, path)
);

-- The asset library below `assets/`; rebuilt from a listing at every start.
CREATE TABLE bop_assets (
    path TEXT PRIMARY KEY,
    size BIGINT NOT NULL,
    etag TEXT,
    updated_at_ms BIGINT NOT NULL
);

-- Packed archives at `exports/<job_id>/<format>.tar`: `packing` (refreshed
-- while in progress) or `ready`.
CREATE TABLE bop_exports (
    job_id TEXT NOT NULL,
    format TEXT NOT NULL,
    state TEXT NOT NULL,
    size BIGINT,
    updated_at_ms BIGINT NOT NULL,
    PRIMARY KEY (job_id, format)
);

-- Whether `bop_job_files` lists every stored file of the job. Jobs from before
-- this migration are indexed from a listing on first use.
ALTER TABLE bop_jobs ADD COLUMN files_indexed BIGINT NOT NULL DEFAULT 0;

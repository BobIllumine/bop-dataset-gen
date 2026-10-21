-- Which server runs a job and until when, so several servers can share the
-- database: a job whose lease runs out is queued again by any of them.
ALTER TABLE bop_jobs ADD COLUMN runner_id TEXT;
ALTER TABLE bop_jobs ADD COLUMN lease_expires_at_ms BIGINT;
-- A cancel requested through any server; the job's own server stops it.
ALTER TABLE bop_jobs ADD COLUMN cancel_requested BIGINT NOT NULL DEFAULT 0;

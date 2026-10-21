//! Job records and the storage index in `SQLite` or `PostgreSQL`, chosen by
//! the database URL.

use std::{
    collections::HashSet,
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use sqlx::{
    AnyPool, Row as _,
    any::{AnyPoolOptions, AnyRow},
    migrate::{Migration, MigrationType, Migrator},
};

/// Where a job is in its lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Waiting for the runner.
    Queued,
    /// Generating scenes.
    Running,
    /// Every scene is committed; final files and the BOP export are being stored.
    Finalizing,
    /// Complete and stored.
    Succeeded,
    /// Stopped by an error; can be resumed.
    Failed,
    /// Stopped on request; can be resumed.
    Cancelled,
}

impl JobState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Finalizing => "finalizing",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Reports whether the job will not change again without a resume.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

impl FromStr for JobState {
    type Err = sqlx::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "finalizing" => Self::Finalizing,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            other => {
                return Err(sqlx::Error::Decode(
                    format!("unknown job state {other:?}").into(),
                ));
            }
        })
    }
}

/// A job as the API reports it; the configuration is fetched separately.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, utoipa::ToSchema)]
pub struct Job {
    /// Random identifier, also the storage prefix below `datasets/`.
    pub id: String,
    /// Lifecycle state.
    pub state: JobState,
    /// `wgpu` or `blenderproc`.
    pub renderer: String,
    /// Whether a BOP export is stored with the dataset.
    pub export_bop: bool,
    /// Scenes the configuration requests.
    pub total_scenes: i64,
    /// Scenes committed so far, including ones from earlier attempts.
    pub committed_scenes: i64,
    /// Why the job failed.
    pub error: Option<String>,
    /// Submission time.
    pub created_at_ms: i64,
    /// Last state or progress change.
    pub updated_at_ms: i64,
    /// When the latest attempt started.
    pub started_at_ms: Option<i64>,
    /// When the job last reached a terminal state.
    pub finished_at_ms: Option<i64>,
    /// A cancel was requested while the job is running; it stops once its
    /// in-flight scenes commit. Not stored: only the running server knows.
    pub cancel_requested: bool,
}

impl Job {
    fn from_row(row: &AnyRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            state: row.try_get::<String, _>("state")?.parse()?,
            renderer: row.try_get("renderer")?,
            export_bop: row.try_get::<i64, _>("export_bop")? != 0,
            total_scenes: row.try_get("total_scenes")?,
            committed_scenes: row.try_get("committed_scenes")?,
            error: row.try_get("error")?,
            created_at_ms: row.try_get("created_at_ms")?,
            updated_at_ms: row.try_get("updated_at_ms")?,
            started_at_ms: row.try_get("started_at_ms")?,
            finished_at_ms: row.try_get("finished_at_ms")?,
            cancel_requested: false,
        })
    }
}

/// A new job's fields.
pub struct NewJob<'a> {
    /// Identifier.
    pub id: &'a str,
    /// Renderer name.
    pub renderer: &'a str,
    /// Whether to store a BOP export.
    pub export_bop: bool,
    /// Resolved configuration text.
    pub config_toml: &'a str,
    /// Requested scene count.
    pub total_scenes: i64,
}

const JOB_COLUMNS: &str = "id, state, renderer, export_bop, total_scenes, committed_scenes, error, \
     created_at_ms, updated_at_ms, started_at_ms, finished_at_ms";

/// Rows per multi-row statement, within every supported bind-parameter limit.
const BATCH_ROWS: usize = 250;

/// How long a `packing` export may go without a refresh before another
/// request takes it over.
pub const EXPORT_STALE_MS: i64 = 30_000;

/// A packed export's record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExportRecord {
    /// Being packed; `updated_at_ms` is the packer's latest refresh.
    Packing {
        /// Latest refresh.
        updated_at_ms: i64,
    },
    /// Stored, `size` bytes.
    Ready {
        /// Archive size in bytes.
        size: u64,
    },
}

/// An asset's index row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetRecord {
    /// Path inside the library.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Storage entity tag; changes whenever the object is replaced.
    pub etag: Option<String>,
}

/// Job persistence and the storage index, shared by the API and the runner.
#[derive(Clone)]
pub struct JobStore {
    pool: AnyPool,
}

impl JobStore {
    /// Connects and applies pending migrations.
    ///
    /// `sqlite://` and `postgres://` URLs are accepted. A `SQLite` URL should
    /// carry `?mode=rwc` to create the file on first start.
    ///
    /// # Errors
    ///
    /// Returns the connection or migration failure.
    pub async fn connect(url: &str) -> Result<Self, sqlx::Error> {
        sqlx::any::install_default_drivers();
        // SQLite allows one writer; a single connection avoids busy errors.
        let connections = if url.starts_with("sqlite:") { 1 } else { 8 };
        let pool = AnyPoolOptions::new()
            .max_connections(connections)
            .connect(url)
            .await?;
        migrator().run(&pool).await?;
        Ok(Self { pool })
    }

    /// Inserts a queued job.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn insert(&self, job: &NewJob<'_>) -> Result<Job, sqlx::Error> {
        let now = now_ms();
        sqlx::query(
            "INSERT INTO bop_jobs (id, state, renderer, export_bop, config_toml, total_scenes, \
             committed_scenes, created_at_ms, updated_at_ms, files_indexed) \
             VALUES ($1, 'queued', $2, $3, $4, $5, 0, $6, $7, 1)",
        )
        .bind(job.id)
        .bind(job.renderer)
        .bind(i64::from(job.export_bop))
        .bind(job.config_toml)
        .bind(job.total_scenes)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        self.get(job.id).await?.ok_or(sqlx::Error::RowNotFound)
    }

    /// Returns one job.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn get(&self, id: &str) -> Result<Option<Job>, sqlx::Error> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {JOB_COLUMNS} FROM bop_jobs WHERE id = $1"
        )))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .as_ref()
        .map(Job::from_row)
        .transpose()
    }

    /// Returns a job's resolved configuration text.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn config(&self, id: &str) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar("SELECT config_toml FROM bop_jobs WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    /// Returns the newest jobs first.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn list(&self, limit: i64) -> Result<Vec<Job>, sqlx::Error> {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT {JOB_COLUMNS} FROM bop_jobs ORDER BY created_at_ms DESC, id LIMIT $1"
        )))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(Job::from_row)
        .collect()
    }

    /// Moves the oldest queued job to running and returns it with its configuration.
    ///
    /// The conditional update makes the claim safe when several servers share
    /// one database: only one of them sees a changed row.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn claim_next(&self) -> Result<Option<(Job, String)>, sqlx::Error> {
        loop {
            let Some(id) = sqlx::query_scalar::<_, String>(
                "SELECT id FROM bop_jobs WHERE state = 'queued' ORDER BY created_at_ms, id LIMIT 1",
            )
            .fetch_optional(&self.pool)
            .await?
            else {
                return Ok(None);
            };
            let now = now_ms();
            let claimed = sqlx::query(
                "UPDATE bop_jobs SET state = 'running', error = NULL, started_at_ms = $1, \
                 updated_at_ms = $2, finished_at_ms = NULL WHERE id = $3 AND state = 'queued'",
            )
            .bind(now)
            .bind(now)
            .bind(&id)
            .execute(&self.pool)
            .await?
            .rows_affected();
            if claimed == 1 {
                let job = self.get(&id).await?.ok_or(sqlx::Error::RowNotFound)?;
                let config = self.config(&id).await?.ok_or(sqlx::Error::RowNotFound)?;
                return Ok(Some((job, config)));
            }
        }
    }

    /// Records the committed scene count.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn set_progress(&self, id: &str, committed: i64) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE bop_jobs SET committed_scenes = $1, updated_at_ms = $2 WHERE id = $3")
            .bind(committed)
            .bind(now_ms())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Moves a job to `state`, recording `error` and, for terminal states, the finish time.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn set_state(
        &self,
        id: &str,
        state: JobState,
        error: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        let now = now_ms();
        sqlx::query(
            "UPDATE bop_jobs SET state = $1, error = $2, updated_at_ms = $3, finished_at_ms = $4 \
             WHERE id = $5",
        )
        .bind(state.as_str())
        .bind(error)
        .bind(now)
        .bind(state.is_terminal().then_some(now))
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Cancels a job that has not started. Returns whether it was queued.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn cancel_queued(&self, id: &str) -> Result<bool, sqlx::Error> {
        let now = now_ms();
        let changed = sqlx::query(
            "UPDATE bop_jobs SET state = 'cancelled', updated_at_ms = $1, finished_at_ms = $2 \
             WHERE id = $3 AND state = 'queued'",
        )
        .bind(now)
        .bind(now)
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(changed == 1)
    }

    /// Queues a failed or cancelled job again. Returns whether it was resumable.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn requeue(&self, id: &str) -> Result<bool, sqlx::Error> {
        let changed = sqlx::query(
            "UPDATE bop_jobs SET state = 'queued', error = NULL, updated_at_ms = $1 \
             WHERE id = $2 AND state IN ('failed', 'cancelled')",
        )
        .bind(now_ms())
        .bind(id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(changed == 1)
    }

    /// Queues every job a previous server process left running; returns how many.
    ///
    /// Only safe when this server is the only runner using the database.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn requeue_interrupted(&self) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query(
            "UPDATE bop_jobs SET state = 'queued', updated_at_ms = $1 \
             WHERE state IN ('running', 'finalizing')",
        )
        .bind(now_ms())
        .execute(&self.pool)
        .await?
        .rows_affected())
    }
}

impl JobStore {
    /// Reports whether `bop_job_files` lists all of a job's stored files, or
    /// `None` for an unknown job.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn files_indexed(&self, id: &str) -> Result<Option<bool>, sqlx::Error> {
        Ok(
            sqlx::query_scalar::<_, i64>("SELECT files_indexed FROM bop_jobs WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
                .map(|indexed| indexed != 0),
        )
    }

    /// Records that `bop_job_files` lists all of a job's stored files.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn mark_files_indexed(&self, id: &str) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE bop_jobs SET files_indexed = 1 WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Records stored result files as `(path, size)`, replacing earlier sizes.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn record_job_files(
        &self,
        id: &str,
        files: &[(String, u64)],
    ) -> Result<(), sqlx::Error> {
        let mut transaction = self.pool.begin().await?;
        for batch in files.chunks(BATCH_ROWS) {
            let sql = format!(
                "INSERT INTO bop_job_files (job_id, path, size) VALUES {} \
                 ON CONFLICT (job_id, path) DO UPDATE SET size = excluded.size",
                placeholders(batch.len(), 3)
            );
            let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
            for (path, size) in batch {
                query = query.bind(id).bind(path.as_str()).bind(to_i64(*size));
            }
            query.execute(&mut *transaction).await?;
        }
        transaction.commit().await
    }

    /// Returns a job's indexed result files as `(path, size)`, in no particular order.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn job_files(&self, id: &str) -> Result<Vec<(String, u64)>, sqlx::Error> {
        sqlx::query("SELECT path, size FROM bop_job_files WHERE job_id = $1")
            .bind(id)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|row| Ok((row.try_get("path")?, to_u64(row.try_get("size")?))))
            .collect()
    }

    /// Replaces the whole asset index with `assets`.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn replace_assets(&self, assets: &[AssetRecord]) -> Result<(), sqlx::Error> {
        let now = now_ms();
        let mut transaction = self.pool.begin().await?;
        sqlx::query("DELETE FROM bop_assets")
            .execute(&mut *transaction)
            .await?;
        for batch in assets.chunks(BATCH_ROWS) {
            let sql = format!(
                "INSERT INTO bop_assets (path, size, etag, updated_at_ms) VALUES {}",
                placeholders(batch.len(), 4)
            );
            let mut query = sqlx::query(sqlx::AssertSqlSafe(sql));
            for asset in batch {
                query = query
                    .bind(asset.path.as_str())
                    .bind(to_i64(asset.size))
                    .bind(asset.etag.as_deref())
                    .bind(now);
            }
            query.execute(&mut *transaction).await?;
        }
        transaction.commit().await
    }

    /// Records one stored asset, replacing an earlier record.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn upsert_asset(&self, asset: &AssetRecord) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO bop_assets (path, size, etag, updated_at_ms) VALUES ($1, $2, $3, $4) \
             ON CONFLICT (path) DO UPDATE SET size = excluded.size, etag = excluded.etag, \
             updated_at_ms = excluded.updated_at_ms",
        )
        .bind(asset.path.as_str())
        .bind(to_i64(asset.size))
        .bind(asset.etag.as_deref())
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Forgets one asset.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn delete_asset(&self, path: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM bop_assets WHERE path = $1")
            .bind(path)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Returns every indexed asset, in no particular order.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn assets(&self) -> Result<Vec<AssetRecord>, sqlx::Error> {
        sqlx::query("SELECT path, size, etag FROM bop_assets")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(asset_from_row)
            .collect()
    }

    /// Returns one asset's record.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn asset(&self, path: &str) -> Result<Option<AssetRecord>, sqlx::Error> {
        sqlx::query("SELECT path, size, etag FROM bop_assets WHERE path = $1")
            .bind(path)
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(asset_from_row)
            .transpose()
    }

    /// Returns which of `paths` are indexed assets.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn present_assets(&self, paths: &[String]) -> Result<HashSet<String>, sqlx::Error> {
        let mut present = HashSet::new();
        for batch in paths.chunks(BATCH_ROWS) {
            let sql = format!(
                "SELECT path FROM bop_assets WHERE path IN ({})",
                (1..=batch.len())
                    .map(|n| format!("${n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let mut query = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(sql));
            for path in batch {
                query = query.bind(path.as_str());
            }
            present.extend(query.fetch_all(&self.pool).await?);
        }
        Ok(present)
    }

    /// Returns the record of a job's packed `format` export.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn export(
        &self,
        id: &str,
        format: &str,
    ) -> Result<Option<ExportRecord>, sqlx::Error> {
        let Some(row) = sqlx::query(
            "SELECT state, size, updated_at_ms FROM bop_exports WHERE job_id = $1 AND format = $2",
        )
        .bind(id)
        .bind(format)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };
        let state: String = row.try_get("state")?;
        Ok(Some(match state.as_str() {
            "packing" => ExportRecord::Packing {
                updated_at_ms: row.try_get("updated_at_ms")?,
            },
            "ready" => ExportRecord::Ready {
                size: to_u64(row.try_get::<Option<i64>, _>("size")?.unwrap_or(0)),
            },
            other => {
                return Err(sqlx::Error::Decode(
                    format!("unknown export state {other:?}").into(),
                ));
            }
        }))
    }

    /// Claims packing an export nobody has started. Returns whether this
    /// caller holds the claim.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn claim_export(&self, id: &str, format: &str) -> Result<bool, sqlx::Error> {
        let claimed = sqlx::query(
            "INSERT INTO bop_exports (job_id, format, state, updated_at_ms) \
             VALUES ($1, $2, 'packing', $3) ON CONFLICT (job_id, format) DO NOTHING",
        )
        .bind(id)
        .bind(format)
        .bind(now_ms())
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(claimed == 1)
    }

    /// Takes over a `packing` export whose packer stopped refreshing it at
    /// `seen_updated_at_ms`. Returns whether this caller now holds the claim.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn take_over_export(
        &self,
        id: &str,
        format: &str,
        seen_updated_at_ms: i64,
    ) -> Result<bool, sqlx::Error> {
        let taken = sqlx::query(
            "UPDATE bop_exports SET updated_at_ms = $1 WHERE job_id = $2 AND format = $3 \
             AND state = 'packing' AND updated_at_ms = $4",
        )
        .bind(now_ms())
        .bind(id)
        .bind(format)
        .bind(seen_updated_at_ms)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(taken == 1)
    }

    /// Refreshes a `packing` export so waiting requests keep waiting.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn refresh_export(&self, id: &str, format: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE bop_exports SET updated_at_ms = $1 \
             WHERE job_id = $2 AND format = $3 AND state = 'packing'",
        )
        .bind(now_ms())
        .bind(id)
        .bind(format)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Records a stored export of `size` bytes.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn set_export_ready(
        &self,
        id: &str,
        format: &str,
        size: u64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO bop_exports (job_id, format, state, size, updated_at_ms) \
             VALUES ($1, $2, 'ready', $3, $4) ON CONFLICT (job_id, format) \
             DO UPDATE SET state = 'ready', size = excluded.size, updated_at_ms = excluded.updated_at_ms",
        )
        .bind(id)
        .bind(format)
        .bind(to_i64(size))
        .bind(now_ms())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Forgets an export, so the next request packs it again.
    ///
    /// # Errors
    ///
    /// Returns the database failure.
    pub async fn delete_export(&self, id: &str, format: &str) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM bop_exports WHERE job_id = $1 AND format = $2")
            .bind(id)
            .bind(format)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

fn asset_from_row(row: &AnyRow) -> Result<AssetRecord, sqlx::Error> {
    Ok(AssetRecord {
        path: row.try_get("path")?,
        size: to_u64(row.try_get("size")?),
        etag: row.try_get("etag")?,
    })
}

/// `($1, $2), ($3, $4), ...` for `rows` rows of `columns` values.
fn placeholders(rows: usize, columns: usize) -> String {
    (0..rows)
        .map(|row| {
            let values = (1..=columns)
                .map(|column| format!("${}", row * columns + column))
                .collect::<Vec<_>>()
                .join(", ");
            format!("({values})")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn migrator() -> Migrator {
    let migration = |version, description: &str, sql: &'static str| {
        Migration::new(
            version,
            description.to_owned().into(),
            MigrationType::Simple,
            sqlx::SqlSafeStr::into_sql_str(sql),
            false,
        )
    };
    Migrator::with_migrations(vec![
        migration(1, "jobs", include_str!("../migrations/0001_jobs.sql")),
        migration(
            2,
            "storage_index",
            include_str!("../migrations/0002_storage_index.sql"),
        ),
    ])
}

/// Current Unix time in milliseconds.
#[must_use]
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

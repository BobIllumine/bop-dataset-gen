//! Job records in `SQLite` or `PostgreSQL`, chosen by the database URL.

use std::{
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

/// Job persistence shared by the API and the runner.
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
             committed_scenes, created_at_ms, updated_at_ms) \
             VALUES ($1, 'queued', $2, $3, $4, $5, 0, $6, $7)",
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

fn migrator() -> Migrator {
    Migrator::with_migrations(vec![Migration::new(
        1,
        "jobs".into(),
        MigrationType::Simple,
        sqlx::SqlSafeStr::into_sql_str(include_str!("../migrations/0001_jobs.sql")),
        false,
    )])
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

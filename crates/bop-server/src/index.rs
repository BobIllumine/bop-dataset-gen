//! The storage index: database rows that answer listings and checks without
//! a storage round trip.
//!
//! Storage stays the source of truth. Rows are written after the object and
//! removed before it, so a crash leaves at worst an unindexed object, and
//! every table can be rebuilt from a listing: the asset index at each start,
//! a job's files on first use for jobs stored before the index existed.

use std::{future::Future, time::Duration};

use thiserror::Error;

use crate::{
    db::{AssetRecord, EXPORT_STALE_MS, ExportRecord, JobStore, now_ms},
    storage::{Storage, StorageError, StoredObject},
};

/// How often a request waiting for another one's export checks on it.
const EXPORT_WAIT_POLL: Duration = Duration::from_millis(250);
/// How often a packer refreshes its claim; well inside [`EXPORT_STALE_MS`].
const EXPORT_REFRESH: Duration = Duration::from_secs(5);

/// An index operation failed.
#[derive(Debug, Error)]
pub enum IndexError {
    /// The database failed.
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    /// Storage failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// A background packing task panicked.
    #[error("export task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

/// Storage key of a job's packed `format` export.
#[must_use]
pub fn export_key(job_id: &str, format: &str) -> String {
    format!("exports/{job_id}/{format}.tar")
}

/// Top-level directory inside a job's `format` export, also its file stem.
#[must_use]
pub fn export_root(job_id: &str, format: &str) -> String {
    format!("{format}-{job_id}")
}

/// Rebuilds the asset index from a listing of `assets/`. Returns the asset count.
///
/// # Errors
///
/// Returns the listing or database failure.
pub async fn reconcile_assets(jobs: &JobStore, storage: &Storage) -> Result<usize, IndexError> {
    let assets = storage
        .list_tagged("assets")
        .await?
        .into_iter()
        .map(|object| AssetRecord {
            path: object.path,
            size: object.size,
            etag: object.etag,
        })
        .collect::<Vec<_>>();
    jobs.replace_assets(&assets).await?;
    Ok(assets.len())
}

/// Indexes a job's stored files from a listing, unless they already are.
///
/// # Errors
///
/// Returns the listing or database failure.
pub async fn ensure_job_files_indexed(
    jobs: &JobStore,
    storage: &Storage,
    job_id: &str,
) -> Result<(), IndexError> {
    if jobs.files_indexed(job_id).await? != Some(false) {
        return Ok(());
    }
    let files = storage
        .list(&format!("datasets/{job_id}"))
        .await?
        .into_iter()
        .map(|object| (object.path, object.size))
        .collect::<Vec<_>>();
    jobs.record_job_files(job_id, &files).await?;
    jobs.mark_files_indexed(job_id).await?;
    Ok(())
}

/// Returns a job's stored result files, sorted by path. An unknown job is
/// answered from a listing, as before the index existed.
///
/// # Errors
///
/// Returns the listing or database failure.
pub async fn job_files(
    jobs: &JobStore,
    storage: &Storage,
    job_id: &str,
) -> Result<Vec<StoredObject>, IndexError> {
    if jobs.files_indexed(job_id).await?.is_none() {
        return Ok(storage.list(&format!("datasets/{job_id}")).await?);
    }
    ensure_job_files_indexed(jobs, storage, job_id).await?;
    let mut files = jobs
        .job_files(job_id)
        .await?
        .into_iter()
        .map(|(path, size)| StoredObject { path, size })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

/// Returns the size of a job's packed `format` export, packing it with `pack`
/// when nobody has. A request that finds another one packing waits for it and
/// shares its archive; one whose packer stopped refreshing takes over. An
/// archive already in storage without a record is recorded, not repacked.
///
/// Packing runs as its own task, so it finishes even if the request that
/// started it goes away.
///
/// # Errors
///
/// Returns the database, storage, or packing failure.
pub async fn ensure_export<F, Fut>(
    jobs: &JobStore,
    storage: &Storage,
    job_id: &str,
    format: &str,
    pack: F,
) -> Result<u64, IndexError>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<u64, StorageError>> + Send,
{
    loop {
        match jobs.export(job_id, format).await? {
            Some(ExportRecord::Ready { size }) => return Ok(size),
            Some(ExportRecord::Packing { updated_at_ms }) => {
                if now_ms().saturating_sub(updated_at_ms) > EXPORT_STALE_MS
                    && jobs.take_over_export(job_id, format, updated_at_ms).await?
                {
                    break;
                }
                tokio::time::sleep(EXPORT_WAIT_POLL).await;
            }
            None => {
                if jobs.claim_export(job_id, format).await? {
                    break;
                }
            }
        }
    }
    let (jobs, storage) = (jobs.clone(), storage.clone());
    let (job_id, format) = (job_id.to_owned(), format.to_owned());
    tokio::spawn(async move {
        let refresher = {
            let (jobs, job_id, format) = (jobs.clone(), job_id.clone(), format.clone());
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(EXPORT_REFRESH).await;
                    if let Err(error) = jobs.refresh_export(&job_id, &format).await {
                        tracing::warn!(job = %job_id, format, "cannot refresh export claim: {error}");
                    }
                }
            })
        };
        let packed = match storage.size(&export_key(&job_id, &format)).await {
            Ok(Some(size)) => Ok(size),
            Ok(None) => pack().await,
            Err(error) => Err(error),
        };
        refresher.abort();
        match packed {
            Ok(size) => {
                jobs.set_export_ready(&job_id, &format, size).await?;
                Ok(size)
            }
            Err(error) => {
                if let Err(db_error) = jobs.delete_export(&job_id, &format).await {
                    tracing::warn!(job = %job_id, format, "cannot release export claim: {db_error}");
                }
                Err(error.into())
            }
        }
    })
    .await?
}

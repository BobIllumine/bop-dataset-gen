//! The storage index: database rows that answer listings and checks without
//! a storage round trip.
//!
//! Storage stays the source of truth. Rows are written after the object and
//! removed before it, so a crash leaves at worst an unindexed object, and
//! every table can be rebuilt from a listing: the asset index at each start,
//! a job's files on first use for jobs stored before the index existed.

use std::{
    collections::{BTreeSet, HashMap},
    future::Future,
    path::Path,
    time::Duration,
};

use futures::StreamExt as _;
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
    /// A local file could not be written.
    #[error("{path}: {source}")]
    Local {
        /// Local path.
        path: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

fn local(path: &Path) -> impl FnOnce(std::io::Error) -> IndexError + '_ {
    move |source| IndexError::Local {
        path: path.display().to_string(),
        source,
    }
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

/// Rebuilds a job's local copy in `dataset` from what storage holds: every
/// scene whose files its `checksums.json` lists are all stored, and the run
/// metadata. Scenes already present locally are left alone. Each scene is
/// assembled in a hidden directory and moved into place whole, and `run.json`
/// comes last, so an interrupted rebuild is simply started again. Returns how
/// many scenes were rebuilt.
///
/// # Errors
///
/// Returns the listing, database, download, or file failure.
pub async fn rebuild_local_copy(
    jobs: &JobStore,
    storage: &Storage,
    job_id: &str,
    dataset: &Path,
) -> Result<usize, IndexError> {
    ensure_job_files_indexed(jobs, storage, job_id).await?;
    let stored = jobs
        .job_files(job_id)
        .await?
        .into_iter()
        .collect::<HashMap<_, _>>();
    if stored.is_empty() {
        return Ok(0);
    }
    let staging = dataset.join(".rebuild");
    if staging.exists() {
        tokio::fs::remove_dir_all(&staging)
            .await
            .map_err(local(&staging))?;
    }
    let scenes = stored
        .keys()
        .filter_map(|path| path.strip_prefix("scenes/")?.split_once('/'))
        .map(|(scene, _)| scene.to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|scene| {
            stored.contains_key(&format!("scenes/{scene}/checksums.json"))
                && !dataset.join("scenes").join(scene).exists()
        })
        .collect::<Vec<_>>();
    let key = |path: &str| format!("datasets/{job_id}/{path}");
    let complete = complete_scenes(storage, &stored, &scenes, &staging, &key).await?;
    let downloads = complete
        .iter()
        .flat_map(|(scene, files)| files.iter().map(move |file| (*scene, file)))
        .collect::<Vec<_>>();
    let fetches = downloads
        .iter()
        .map(|(scene, file)| {
            let target = staging.join(scene).join(file);
            let source = key(&format!("scenes/{scene}/{file}"));
            async move { storage.download(&source, &target).await }
        })
        .collect::<Vec<_>>();
    for fetched in futures::stream::iter(fetches)
        .buffer_unordered(storage.concurrency())
        .collect::<Vec<_>>()
        .await
    {
        fetched?;
    }
    let scenes_dir = dataset.join("scenes");
    tokio::fs::create_dir_all(&scenes_dir)
        .await
        .map_err(local(&scenes_dir))?;
    for (scene, _) in &complete {
        let target = scenes_dir.join(scene);
        tokio::fs::rename(staging.join(scene), &target)
            .await
            .map_err(local(&target))?;
    }
    for name in ["environment.json", "resolved-config.toml", "run.json"] {
        if stored.contains_key(name) {
            storage.download(&key(name), &dataset.join(name)).await?;
        }
    }
    if staging.exists() {
        tokio::fs::remove_dir_all(&staging)
            .await
            .map_err(local(&staging))?;
    }
    Ok(complete.len())
}

/// The `scenes` whose files, as their stored `checksums.json` lists them, are
/// all in `stored`, with those files. Each listing is downloaded to `staging`.
async fn complete_scenes<'a>(
    storage: &Storage,
    stored: &HashMap<String, u64>,
    scenes: &'a [String],
    staging: &Path,
    key: &impl Fn(&str) -> String,
) -> Result<Vec<(&'a String, Vec<String>)>, IndexError> {
    // Futures built up front: a mapping closure would make the caller's future not `Send`.
    let listings = scenes
        .iter()
        .map(|scene| async move {
            let target = staging.join(scene).join("checksums.json");
            storage
                .download(&key(&format!("scenes/{scene}/checksums.json")), &target)
                .await?;
            let bytes = tokio::fs::read(&target).await.map_err(local(&target))?;
            let files: Option<Vec<String>> = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|checksums| {
                    Some(
                        checksums
                            .get("files")?
                            .as_object()?
                            .keys()
                            .cloned()
                            .collect(),
                    )
                });
            Ok::<_, IndexError>((scene, files))
        })
        .collect::<Vec<_>>();
    let mut complete = Vec::new();
    for listed in futures::stream::iter(listings)
        .buffer_unordered(storage.concurrency())
        .collect::<Vec<_>>()
        .await
    {
        let (scene, files) = listed?;
        if let Some(files) = files
            && files
                .iter()
                .all(|file| stored.contains_key(&format!("scenes/{scene}/{file}")))
        {
            complete.push((scene, files));
        }
    }
    Ok(complete)
}

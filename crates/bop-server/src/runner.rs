//! Runs queued jobs one at a time and stores their results.
//!
//! A job generates into `<data_dir>/jobs/<id>/dataset`; each committed scene
//! is uploaded to `datasets/<id>/` as soon as it is published, and the
//! remaining files after the run. The local copy is what resume continues
//! from, so a restarted or resumed job only renders the missing scenes, and
//! only uploads the scenes storage does not hold yet. Finishing packs the
//! export archives from the local copy.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use bop_pipeline::CancellationToken;
use bop_run::{GenerateOptions, LoadedRun};
use futures::StreamExt as _;
use serde::Serialize;
use tokio::sync::{Notify, broadcast, mpsc};

use crate::{
    db::{Job, JobState, JobStore},
    index,
    settings::Settings,
    storage::Storage,
    submission,
};

/// How often an idle runner checks the database for jobs other servers queued.
const IDLE_POLL: Duration = Duration::from_secs(5);

/// A change to one job, broadcast to event-stream subscribers.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
pub struct JobEvent {
    /// Job identifier.
    pub job_id: String,
    /// State after the change.
    pub state: JobState,
    /// Scenes committed so far.
    pub committed_scenes: i64,
    /// Requested scenes.
    pub total_scenes: i64,
    /// Failure message, for a failed job.
    pub error: Option<String>,
    /// A cancel is pending; see [`Job::cancel_requested`].
    pub cancel_requested: bool,
}

/// The job this server is running and how to stop it.
struct Active {
    job_id: String,
    cancellation: CancellationToken,
}

/// Shared runner handle: wakes the loop and cancels the active job.
#[derive(Clone)]
pub struct Runner {
    inner: Arc<Inner>,
}

struct Inner {
    jobs: JobStore,
    storage: Storage,
    settings: Arc<Settings>,
    events: broadcast::Sender<JobEvent>,
    wake: Notify,
    active: Mutex<Option<Active>>,
    shutdown: CancellationToken,
}

impl Runner {
    /// Creates a runner; call [`Self::run`] to start processing.
    #[must_use]
    pub fn new(jobs: JobStore, storage: Storage, settings: Arc<Settings>) -> Self {
        let (events, _) = broadcast::channel(1024);
        Self {
            inner: Arc::new(Inner {
                jobs,
                storage,
                settings,
                events,
                wake: Notify::new(),
                active: Mutex::new(None),
                shutdown: CancellationToken::new(),
            }),
        }
    }

    /// Subscribes to job events.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<JobEvent> {
        self.inner.events.subscribe()
    }

    /// Publishes a job's current state to subscribers.
    pub fn announce(&self, job: &Job) {
        let _ = self.inner.events.send(JobEvent {
            job_id: job.id.clone(),
            state: job.state,
            committed_scenes: job.committed_scenes,
            total_scenes: job.total_scenes,
            error: job.error.clone(),
            cancel_requested: self.is_cancelling(&job.id),
        });
    }

    /// Reports whether `job_id` is running here with a cancel pending.
    #[must_use]
    pub fn is_cancelling(&self, job_id: &str) -> bool {
        self.inner
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|active| active.job_id == job_id && active.cancellation.is_cancelled())
    }

    /// Fills in what only the runner knows about `job`.
    #[must_use]
    pub fn describe(&self, mut job: Job) -> Job {
        job.cancel_requested = self.is_cancelling(&job.id);
        job
    }

    /// Wakes the loop after a job was queued.
    pub fn wake(&self) {
        self.inner.wake.notify_one();
    }

    /// Asks the job running here to stop before its next scene.
    /// Returns whether `job_id` is running on this server.
    pub fn cancel(&self, job_id: &str) -> bool {
        let active = self
            .inner
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match active.as_ref() {
            Some(active) if active.job_id == job_id => {
                active.cancellation.cancel();
                true
            }
            _ => false,
        }
    }

    /// Stops the active job before its next scene and stops claiming jobs.
    /// The interrupted job is queued again rather than marked cancelled.
    pub fn shutdown(&self) {
        self.inner.shutdown.cancel();
        if let Some(active) = self
            .inner
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            active.cancellation.cancel();
        }
        self.inner.wake.notify_one();
    }

    /// Claims and runs jobs until [`Self::shutdown`].
    pub async fn run(self) {
        while !self.inner.shutdown.is_cancelled() {
            match self.inner.jobs.claim_next().await {
                Ok(Some((job, config))) => self.run_job(job, config).await,
                Ok(None) => {
                    let _ = tokio::time::timeout(IDLE_POLL, self.inner.wake.notified()).await;
                }
                Err(error) => {
                    tracing::error!("cannot claim a job: {error}");
                    tokio::time::sleep(IDLE_POLL).await;
                }
            }
        }
    }

    async fn run_job(&self, mut job: Job, config: String) {
        let cancellation = CancellationToken::new();
        *self
            .inner
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Active {
            job_id: job.id.clone(),
            cancellation: cancellation.clone(),
        });
        tracing::info!(job = %job.id, renderer = %job.renderer, "starting job");
        self.announce(&job);
        let outcome = self.execute(&mut job, config, cancellation).await;
        *self
            .inner
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        let (state, error) = match outcome {
            Ok(Outcome::Succeeded) => (JobState::Succeeded, None),
            Ok(Outcome::Stopped) if self.inner.shutdown.is_cancelled() => (JobState::Queued, None),
            Ok(Outcome::Stopped) => (JobState::Cancelled, None),
            Err(message) => (JobState::Failed, Some(message)),
        };
        if let Some(message) = &error {
            tracing::error!(job = %job.id, "job failed: {message}");
        } else {
            tracing::info!(job = %job.id, state = ?state, "job finished");
        }
        if let Err(db_error) = self
            .inner
            .jobs
            .set_state(&job.id, state, error.as_deref())
            .await
        {
            tracing::error!(job = %job.id, "cannot record job state: {db_error}");
        }
        job.state = state;
        job.error = error;
        self.announce(&job);
    }

    async fn execute(
        &self,
        job: &mut Job,
        config: String,
        cancellation: CancellationToken,
    ) -> Result<Outcome, String> {
        let settings = &self.inner.settings;
        let work = settings.jobs_dir().join(&job.id);
        let project = work.join("project");
        let dataset = work.join("dataset");
        let renderer = submission::parse_renderer(&job.renderer)?;
        let accepted = submission::accept(&config, settings)?;
        self.fetch_assets(&accepted.asset_paths, &project).await?;
        let loaded = LoadedRun::parse(accepted.resolved_toml.into_bytes(), &project)
            .map_err(|error| format!("invalid config: {error}"))?;

        job.committed_scenes = count_committed(&dataset);
        self.record_progress(job).await;
        let mut uploaded = self.stored_scene_files(&job.id).await?;

        let (scenes, mut committed) = mpsc::unbounded_channel::<PathBuf>();
        let options = GenerateOptions {
            cancellation,
            observer: Some(Arc::new(move |scene: &bop_pipeline::CompletedScene| {
                let _ = scenes.send(scene.scene_path().to_owned());
            })),
            blenderproc: settings.blenderproc.clone(),
        };
        let output = dataset.clone();
        let generation = tokio::task::spawn_blocking(move || {
            let prepared =
                bop_pipeline::prepare_asset_catalog(loaded.config().plan(), loaded.directory())
                    .map_err(|error| format!("asset preparation failed: {error}"))?;
            bop_run::generate(&loaded, &prepared, renderer, &output, &options)
                .map_err(|error| error.to_string())
        });

        let mut upload_error = None;
        while let Some(first) = committed.recv().await {
            // Scenes that committed during the last upload go up together.
            let mut scene_paths = vec![first];
            while let Ok(scene_path) = committed.try_recv() {
                scene_paths.push(scene_path);
            }
            job.committed_scenes += i64::try_from(scene_paths.len()).unwrap_or(i64::MAX);
            self.record_progress(job).await;
            if upload_error.is_none()
                && let Err(error) = self
                    .upload_trees(&job.id, &dataset, &scene_paths, &mut uploaded)
                    .await
            {
                upload_error = Some(error);
            }
        }
        let report = generation
            .await
            .map_err(|error| format!("generation task panicked: {error}"))??;
        if let Some(error) = upload_error {
            return Err(error);
        }

        // Scenes committed before a restart, and the run metadata.
        self.upload_trees(
            &job.id,
            &dataset,
            std::slice::from_ref(&dataset),
            &mut uploaded,
        )
        .await?;
        if report.was_cancelled() {
            return Ok(Outcome::Stopped);
        }

        self.inner
            .jobs
            .set_state(&job.id, JobState::Finalizing, None)
            .await
            .map_err(|error| error.to_string())?;
        job.state = JobState::Finalizing;
        self.announce(job);
        self.finalize(job, &work, &dataset).await?;
        if !settings.keep_local
            && let Err(error) = tokio::fs::remove_dir_all(&work).await
        {
            tracing::warn!(job = %job.id, "cannot remove {}: {error}", work.display());
        }
        Ok(Outcome::Succeeded)
    }

    /// Validates the dataset, then packs and stores its export, and the BOP
    /// export when requested.
    async fn finalize(&self, job: &Job, work: &Path, dataset: &Path) -> Result<(), String> {
        let root = dataset.to_owned();
        let validation = tokio::task::spawn_blocking(move || bop_dataset::validate_dataset(&root))
            .await
            .map_err(|error| format!("validation task panicked: {error}"))?
            .map_err(|error| format!("dataset validation failed: {error}"))?;
        tracing::info!(
            job = %job.id,
            scenes = validation.scene_count(),
            files = validation.file_count(),
            "validated dataset"
        );
        self.store_export(job, "dataset", dataset).await?;
        if !job.export_bop {
            return Ok(());
        }
        let bop = work.join("bop");
        if bop.exists() {
            tokio::fs::remove_dir_all(&bop)
                .await
                .map_err(|error| format!("cannot clear {}: {error}", bop.display()))?;
        }
        let (source, target) = (dataset.to_owned(), bop.clone());
        tokio::task::spawn_blocking(move || {
            bop_dataset::export_bop(&source, &target, bop_dataset::BopExportOptions::default())
        })
        .await
        .map_err(|error| format!("BOP export task panicked: {error}"))?
        .map_err(|error| format!("BOP export failed: {error}"))?;
        // Only the archive is stored; the dataset already holds the same data.
        self.store_export(job, "bop", &bop).await
    }

    /// Packs every file below `tree` into the job's `format` export archive,
    /// entry for entry what the export API packs from storage.
    async fn store_export(&self, job: &Job, format: &str, tree: &Path) -> Result<(), String> {
        let files = list_files_blocking(tree, &[tree.to_owned()]).await?;
        let size = self
            .inner
            .storage
            .pack_local_tar(
                tree,
                &files,
                &index::export_root(&job.id, format),
                &index::export_key(&job.id, format),
            )
            .await
            .map_err(|error| format!("cannot store the {format} export: {error}"))?;
        self.inner
            .jobs
            .set_export_ready(&job.id, format, size)
            .await
            .map_err(|error| format!("cannot record the {format} export: {error}"))
    }

    /// Fetches the run's assets into `project`, several at a time.
    async fn fetch_assets(&self, paths: &[String], project: &Path) -> Result<(), String> {
        let cache = self.inner.settings.asset_cache_dir();
        // Futures built up front: a mapping closure would make the runner's future not `Send`.
        let fetches = paths
            .iter()
            .map(|path| self.fetch_asset(path, project, &cache))
            .collect::<Vec<_>>();
        let outcomes = futures::stream::iter(fetches)
            .buffer_unordered(self.inner.storage.concurrency())
            .collect::<Vec<_>>()
            .await;
        outcomes.into_iter().collect()
    }

    /// Links one asset from the shared cache, downloading it there first when
    /// this version (by entity tag) is not cached yet.
    async fn fetch_asset(&self, path: &str, project: &Path, cache: &Path) -> Result<(), String> {
        let local = project.join(path);
        // Kept from an earlier attempt so a resumed run sees the same bytes.
        if local.is_file() {
            return Ok(());
        }
        let fail = |error: &dyn std::fmt::Display| format!("cannot fetch asset {path}: {error}");
        let key = format!("assets/{path}");
        let storage = &self.inner.storage;
        let record = self.inner.jobs.asset(path).await.map_err(|e| fail(&e))?;
        let Some(cached) = record.and_then(|record| {
            let etag = record.etag?;
            Some(cache.join(cache_entry(&etag, record.size)).join(path))
        }) else {
            return storage.download(&key, &local).await.map_err(|e| fail(&e));
        };
        if !cached.is_file() {
            storage
                .download(&key, &cached)
                .await
                .map_err(|e| fail(&e))?;
        }
        link_or_copy(&cached, &local).await.map_err(|e| fail(&e))
    }

    /// Scene files an earlier attempt already stored, by path and size. Other
    /// files are rewritten by every attempt, so they are always uploaded.
    async fn stored_scene_files(&self, job_id: &str) -> Result<HashMap<String, u64>, String> {
        let fail = |error: &dyn std::fmt::Display| format!("cannot read stored files: {error}");
        index::ensure_job_files_indexed(&self.inner.jobs, &self.inner.storage, job_id)
            .await
            .map_err(|e| fail(&e))?;
        Ok(self
            .inner
            .jobs
            .job_files(job_id)
            .await
            .map_err(|e| fail(&e))?
            .into_iter()
            .filter(|(path, _)| path.starts_with("scenes/"))
            .collect())
    }

    /// Uploads every file below `trees` that `uploaded` does not list with its
    /// current size, keyed by its path relative to `root` under
    /// `datasets/<job_id>/`, several at a time. Hidden entries (the private
    /// staging area) are skipped. Whatever was stored is indexed and added to
    /// `uploaded`, even when another file failed.
    async fn upload_trees(
        &self,
        job_id: &str,
        root: &Path,
        trees: &[PathBuf],
        uploaded: &mut HashMap<String, u64>,
    ) -> Result<(), String> {
        let pending = list_files_blocking(root, trees)
            .await?
            .into_iter()
            .filter(|(relative, size)| uploaded.get(relative) != Some(size))
            .collect::<Vec<_>>();
        let storage = &self.inner.storage;
        let uploads = pending
            .into_iter()
            .map(|(relative, size)| async move {
                let stored = storage
                    .put_file(
                        &format!("datasets/{job_id}/{relative}"),
                        &root.join(&relative),
                    )
                    .await;
                (relative, size, stored)
            })
            .collect::<Vec<_>>();
        let results = futures::stream::iter(uploads)
            .buffer_unordered(storage.concurrency())
            .collect::<Vec<_>>()
            .await;
        let mut stored = Vec::with_capacity(results.len());
        let mut failure = None;
        for (relative, size, result) in results {
            match result {
                Ok(()) => stored.push((relative, size)),
                Err(error) => {
                    failure.get_or_insert_with(|| format!("cannot store {relative}: {error}"));
                }
            }
        }
        if !stored.is_empty() {
            self.inner
                .jobs
                .record_job_files(job_id, &stored)
                .await
                .map_err(|error| format!("cannot index stored files: {error}"))?;
        }
        uploaded.extend(stored);
        failure.map_or(Ok(()), Err)
    }

    async fn record_progress(&self, job: &Job) {
        if let Err(error) = self
            .inner
            .jobs
            .set_progress(&job.id, job.committed_scenes)
            .await
        {
            tracing::warn!(job = %job.id, "cannot record progress: {error}");
        }
        self.announce(job);
    }
}

enum Outcome {
    Succeeded,
    Stopped,
}

/// Counts canonical scene directories already published in `dataset`.
fn count_committed(dataset: &Path) -> i64 {
    std::fs::read_dir(dataset.join("scenes")).map_or(0, |entries| {
        let count = entries
            .filter_map(Result::ok)
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .count();
        i64::try_from(count).unwrap_or(i64::MAX)
    })
}

/// Directory name in the asset cache for one version of an asset.
fn cache_entry(etag: &str, size: u64) -> String {
    let etag = etag
        .trim_matches('"')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("{etag}-{size}")
}

/// Hard-links `source` to `target`, creating parent directories, or copies it
/// when they are on different file systems.
async fn link_or_copy(source: &Path, target: &Path) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    if tokio::fs::hard_link(source, target).await.is_ok() {
        return Ok(());
    }
    tokio::fs::copy(source, target).await.map(|_| ())
}

/// [`list_files`] for each of `trees`, off the async runtime, sorted by path.
async fn list_files_blocking(root: &Path, trees: &[PathBuf]) -> Result<Vec<(String, u64)>, String> {
    let (root, trees) = (root.to_owned(), trees.to_vec());
    tokio::task::spawn_blocking(move || {
        let mut files = Vec::new();
        for tree in &trees {
            files.extend(list_files(&root, tree)?);
        }
        files.sort();
        Ok(files)
    })
    .await
    .map_err(|error| format!("file listing panicked: {error}"))?
}

/// Lists files below `tree` as `/`-separated paths relative to `root` with
/// their sizes, sorted by path, skipping hidden entries.
fn list_files(root: &Path, tree: &Path) -> Result<Vec<(String, u64)>, String> {
    let mut files = Vec::new();
    let mut pending = vec![tree.to_owned()];
    while let Some(directory) = pending.pop() {
        let entries = std::fs::read_dir(&directory)
            .map_err(|error| format!("cannot list {}: {error}", directory.display()))?;
        for entry in entries {
            let entry =
                entry.map_err(|error| format!("cannot list {}: {error}", directory.display()))?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let path = entry.path();
            let kind = entry
                .file_type()
                .map_err(|error| format!("cannot stat {}: {error}", path.display()))?;
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                let size = entry
                    .metadata()
                    .map_err(|error| format!("cannot stat {}: {error}", path.display()))?
                    .len();
                let relative = path
                    .strip_prefix(root)
                    .expect("listed below the root")
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                files.push((relative, size));
            }
        }
    }
    files.sort();
    Ok(files)
}

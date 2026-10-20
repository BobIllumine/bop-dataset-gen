//! Runs queued jobs one at a time and stores their results.
//!
//! A job generates into `<data_dir>/jobs/<id>/dataset`; each committed scene
//! is uploaded to `datasets/<id>/` as soon as it is published, and the
//! remaining files after the run. The local copy is what resume continues
//! from, so a restarted or resumed job only renders the missing scenes.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use bop_pipeline::CancellationToken;
use bop_run::{GenerateOptions, LoadedRun};
use serde::Serialize;
use tokio::sync::{Notify, broadcast, mpsc};

use crate::{
    db::{Job, JobState, JobStore},
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

        let mut uploaded = HashSet::new();
        let mut upload_error = None;
        while let Some(scene_path) = committed.recv().await {
            job.committed_scenes += 1;
            self.record_progress(job).await;
            if upload_error.is_none()
                && let Err(error) = self
                    .upload_tree(&job.id, &dataset, &scene_path, &mut uploaded)
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
        self.upload_tree(&job.id, &dataset, &dataset, &mut uploaded)
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

    /// Validates the dataset, then exports and stores BOP when requested.
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
        let mut uploaded = HashSet::new();
        self.upload_tree(&format!("{}/bop", job.id), &bop, &bop, &mut uploaded)
            .await
    }

    async fn fetch_assets(&self, paths: &[String], project: &Path) -> Result<(), String> {
        for path in paths {
            let local = project.join(path);
            // Kept from an earlier attempt so a resumed run sees the same bytes.
            if local.is_file() {
                continue;
            }
            self.inner
                .storage
                .download(&format!("assets/{path}"), &local)
                .await
                .map_err(|error| format!("cannot fetch asset {path}: {error}"))?;
        }
        Ok(())
    }

    /// Uploads every file below `tree` not yet in `uploaded`, keyed by its
    /// path relative to `root` under `datasets/<prefix>/`. Hidden entries
    /// (the private staging area) are skipped.
    async fn upload_tree(
        &self,
        prefix: &str,
        root: &Path,
        tree: &Path,
        uploaded: &mut HashSet<String>,
    ) -> Result<(), String> {
        let (root_owned, tree_owned) = (root.to_owned(), tree.to_owned());
        let files = tokio::task::spawn_blocking(move || list_files(&root_owned, &tree_owned))
            .await
            .map_err(|error| format!("file listing panicked: {error}"))??;
        for relative in files {
            if uploaded.contains(&relative) {
                continue;
            }
            self.inner
                .storage
                .put_file(
                    &format!("datasets/{prefix}/{relative}"),
                    &root.join(&relative),
                )
                .await
                .map_err(|error| format!("cannot store {relative}: {error}"))?;
            uploaded.insert(relative);
        }
        Ok(())
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

/// Lists files below `tree` as `/`-separated paths relative to `root`, skipping hidden entries.
fn list_files(root: &Path, tree: &Path) -> Result<Vec<String>, String> {
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
                let relative = path
                    .strip_prefix(root)
                    .expect("listed below the root")
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                files.push(relative);
            }
        }
    }
    files.sort();
    Ok(files)
}

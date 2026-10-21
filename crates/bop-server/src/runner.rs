//! Runs queued jobs one at a time and stores their results.
//!
//! A job generates into `<data_dir>/jobs/<id>/dataset`; each committed scene
//! is uploaded to `datasets/<id>/` as soon as it is published, and the
//! remaining files after the run. The local copy is what resume continues
//! from, so a restarted or resumed job only renders the missing scenes, and
//! only uploads the scenes storage does not hold yet. Finishing packs the
//! export archives from the local copy.
//!
//! Several servers may share one database. A running job is leased to its
//! server, which renews the lease while it works; any server queues a job
//! whose lease ran out again. The server that picks it up rebuilds the local
//! copy from storage when it has none, so no committed scene is rendered
//! twice. On `PostgreSQL`, servers hear of each other's job changes at once.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use bop_pipeline::CancellationToken;
use bop_run::{GenerateOptions, LoadedRun};
use futures::StreamExt as _;
use serde::Serialize;
use tokio::sync::{Notify, broadcast, mpsc};

use crate::{
    db::{JOB_CHANNEL, Job, JobState, JobStore, now_ms},
    index,
    settings::Settings,
    storage::Storage,
    submission,
};

/// How often an idle runner checks the database for jobs other servers queued.
const IDLE_POLL: Duration = Duration::from_secs(5);
/// How long a claim on a running job lasts unless renewed.
const LEASE_MS: i64 = 60_000;
/// How often the server running a job renews its lease and checks for a cancel.
const LEASE_RENEWAL: Duration = Duration::from_secs(10);
/// How often a server releases jobs whose lease ran out.
const LEASE_CHECK: Duration = Duration::from_secs(15);
/// How often old local copies and stale cache entries are deleted.
const RETENTION_SWEEP: Duration = Duration::from_hours(1);
/// Top-level run files that every attempt writes identically; stored once.
const RUN_METADATA: [&str; 3] = ["run.json", "environment.json", "resolved-config.toml"];

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
    /// Held while claiming a job and while deleting local copies, so a copy
    /// is never deleted under a job that just claimed it.
    disk: tokio::sync::Mutex<()>,
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
                disk: tokio::sync::Mutex::new(()),
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
            cancel_requested: job.cancel_requested || self.is_cancelling(&job.id),
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
        job.cancel_requested |= self.is_cancelling(&job.id);
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

    /// Claims and runs jobs until [`Self::shutdown`]. Meanwhile releases jobs
    /// whose lease ran out, deletes old local copies, and on `PostgreSQL`
    /// follows other servers' job changes.
    pub async fn run(self) {
        let background = [
            tokio::spawn(self.clone().maintain()),
            tokio::spawn(self.clone().listen()),
        ];
        while !self.inner.shutdown.is_cancelled() {
            match self.claim().await {
                Ok(Some((job, config, cancellation))) => {
                    self.run_job(job, config, cancellation).await;
                }
                Ok(None) => {
                    let _ = tokio::time::timeout(IDLE_POLL, self.inner.wake.notified()).await;
                }
                Err(error) => {
                    tracing::error!("cannot claim a job: {error}");
                    tokio::time::sleep(IDLE_POLL).await;
                }
            }
        }
        for task in background {
            task.abort();
        }
    }

    /// Claims the next job and marks it active here, under the disk lock.
    async fn claim(&self) -> Result<Option<(Job, String, CancellationToken)>, sqlx::Error> {
        let _disk = self.inner.disk.lock().await;
        let Some((job, config)) = self
            .inner
            .jobs
            .claim_next(&self.inner.settings.runner_id, LEASE_MS)
            .await?
        else {
            return Ok(None);
        };
        let cancellation = CancellationToken::new();
        *self
            .inner
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Active {
            job_id: job.id.clone(),
            cancellation: cancellation.clone(),
        });
        Ok(Some((job, config, cancellation)))
    }

    async fn run_job(&self, mut job: Job, config: String, cancellation: CancellationToken) {
        tracing::info!(job = %job.id, renderer = %job.renderer, "starting job");
        self.announce(&job);
        let lost = Arc::new(AtomicBool::new(false));
        let lease = tokio::spawn(self.clone().hold_lease(
            job.id.clone(),
            cancellation.clone(),
            Arc::clone(&lost),
        ));
        let outcome = self.execute(&mut job, config, cancellation).await;
        lease.abort();
        *self
            .inner
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
        if lost.load(Ordering::SeqCst) {
            tracing::warn!(job = %job.id, "lost the job's lease; another server continues it");
            return;
        }
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
        match self
            .inner
            .jobs
            .set_runner_state(
                &job.id,
                &self.inner.settings.runner_id,
                state,
                error.as_deref(),
            )
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(job = %job.id, "lost the job's lease before recording its state");
            }
            Err(db_error) => tracing::error!(job = %job.id, "cannot record job state: {db_error}"),
        }
        if let Ok(Some(stored)) = self.inner.jobs.get(&job.id).await {
            self.announce(&stored);
        }
    }

    /// Renews the active job's lease until aborted. A requested cancel stops
    /// the job; a lost lease stops it without recording anything.
    async fn hold_lease(
        self,
        job_id: String,
        cancellation: CancellationToken,
        lost: Arc<AtomicBool>,
    ) {
        let runner_id = &self.inner.settings.runner_id;
        loop {
            tokio::time::sleep(LEASE_RENEWAL).await;
            match self
                .inner
                .jobs
                .renew_lease(&job_id, runner_id, LEASE_MS)
                .await
            {
                Ok(Some(cancel)) => {
                    if cancel {
                        cancellation.cancel();
                    }
                }
                Ok(None) => {
                    lost.store(true, Ordering::SeqCst);
                    cancellation.cancel();
                    return;
                }
                Err(error) => {
                    tracing::warn!(job = %job_id, "cannot renew the job's lease: {error}");
                }
            }
        }
    }

    /// Releases jobs whose lease ran out and, hourly, deletes old local data.
    async fn maintain(self) {
        let mut last_sweep: Option<Instant> = None;
        loop {
            match self.inner.jobs.release_interrupted(None).await {
                Ok(released) => {
                    for id in &released {
                        tracing::info!(job = %id, "released a job whose lease ran out");
                        if let Ok(Some(job)) = self.inner.jobs.get(id).await {
                            self.announce(&job);
                        }
                    }
                    if !released.is_empty() {
                        self.wake();
                    }
                }
                Err(error) => tracing::warn!("cannot release interrupted jobs: {error}"),
            }
            if self.inner.settings.local_retention_days > 0
                && last_sweep.is_none_or(|at| at.elapsed() >= RETENTION_SWEEP)
            {
                self.sweep_local_data().await;
                last_sweep = Some(Instant::now());
            }
            tokio::time::sleep(LEASE_CHECK).await;
        }
    }

    /// Follows [`JOB_CHANNEL`] on `PostgreSQL`: announces other servers' job
    /// changes to this server's subscribers, wakes for queued jobs, and stops
    /// the active job when a cancel for it arrives.
    async fn listen(self) {
        let url = &self.inner.settings.database_url;
        if !(url.starts_with("postgres:") || url.starts_with("postgresql:")) {
            return;
        }
        loop {
            let mut listener = match sqlx::postgres::PgListener::connect(url).await {
                Ok(listener) => listener,
                Err(error) => {
                    tracing::warn!("cannot listen for job changes: {error}");
                    tokio::time::sleep(IDLE_POLL).await;
                    continue;
                }
            };
            if let Err(error) = listener.listen(JOB_CHANNEL).await {
                tracing::warn!("cannot listen for job changes: {error}");
                tokio::time::sleep(IDLE_POLL).await;
                continue;
            }
            loop {
                match listener.recv().await {
                    Ok(notification) => self.on_job_change(notification.payload()).await,
                    // The listener reconnects on the next call; changes in between are missed.
                    Err(error) => {
                        tracing::warn!("lost the job-change listener: {error}");
                        tokio::time::sleep(IDLE_POLL).await;
                    }
                }
            }
        }
    }

    async fn on_job_change(&self, payload: &str) {
        let Some((origin, id)) = payload.split_once(' ') else {
            return;
        };
        // This process already announced its own changes.
        if origin == self.inner.jobs.origin() {
            return;
        }
        let Ok(Some(job)) = self.inner.jobs.get(id).await else {
            return;
        };
        self.announce(&job);
        if job.state == JobState::Queued {
            self.wake();
        }
        if job.cancel_requested {
            self.cancel(id);
        }
    }

    /// Deletes local copies of failed and cancelled jobs untouched for the
    /// retention period (resume rebuilds them from storage), copies of jobs
    /// the database no longer knows, and asset-cache entries for asset
    /// versions the library no longer holds.
    async fn sweep_local_data(&self) {
        let settings = &self.inner.settings;
        let retention_ms = i64::from(settings.local_retention_days) * 86_400_000;
        let cutoff = now_ms().saturating_sub(retention_ms);
        let _disk = self.inner.disk.lock().await;
        let active = self
            .inner
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|active| active.job_id.clone());
        let mut removed = 0_usize;
        for (id, path, modified_ms) in local_entries(&settings.jobs_dir()).await {
            if active.as_deref() == Some(id.as_str()) {
                continue;
            }
            let expired = match self.inner.jobs.get(&id).await {
                Ok(Some(job)) => {
                    matches!(job.state, JobState::Failed | JobState::Cancelled)
                        && job.updated_at_ms < cutoff
                }
                Ok(None) => modified_ms < cutoff,
                Err(error) => {
                    tracing::warn!("cannot check local job copies: {error}");
                    return;
                }
            };
            if expired {
                match tokio::fs::remove_dir_all(&path).await {
                    Ok(()) => removed += 1,
                    Err(error) => tracing::warn!("cannot remove {}: {error}", path.display()),
                }
            }
        }
        let assets = match self.inner.jobs.assets().await {
            Ok(assets) => assets
                .into_iter()
                .filter_map(|asset| Some((asset.path, cache_entry(&asset.etag?, asset.size))))
                .collect::<HashMap<_, _>>(),
            Err(error) => {
                tracing::warn!("cannot check the asset cache: {error}");
                return;
            }
        };
        let cache = settings.asset_cache_dir();
        for (entry, path, _) in local_entries(&cache).await {
            let Ok(files) = list_files_blocking(&path, std::slice::from_ref(&path)).await else {
                continue;
            };
            let mut kept = 0_usize;
            for (relative, _) in files {
                if assets.get(&relative) == Some(&entry) {
                    kept += 1;
                } else if tokio::fs::remove_file(path.join(&relative)).await.is_ok() {
                    removed += 1;
                }
            }
            if kept == 0 {
                let _ = tokio::fs::remove_dir_all(&path).await;
            }
        }
        if removed > 0 {
            tracing::info!(removed, "deleted old local data");
        }
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

        // A job continued from another server, or after its local copy was deleted.
        if !dataset.join("run.json").is_file() {
            let rebuilt =
                index::rebuild_local_copy(&self.inner.jobs, &self.inner.storage, &job.id, &dataset)
                    .await
                    .map_err(|error| {
                        format!("cannot rebuild the local copy from storage: {error}")
                    })?;
            if rebuilt > 0 {
                tracing::info!(job = %job.id, scenes = rebuilt, "rebuilt the local copy from storage");
            }
        }
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
        // The run metadata goes up with the first scenes, so another server can
        // continue the run from storage.
        let mut metadata = RUN_METADATA.map(|name| dataset.join(name)).to_vec();
        while let Some(first) = committed.recv().await {
            // Scenes that committed during the last upload go up together.
            let mut scene_paths = vec![first];
            while let Ok(scene_path) = committed.try_recv() {
                scene_paths.push(scene_path);
            }
            job.committed_scenes += i64::try_from(scene_paths.len()).unwrap_or(i64::MAX);
            self.record_progress(job).await;
            scene_paths.append(&mut metadata);
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

        // Scenes committed before a restart, and the files written after the
        // run, which may have changed since an earlier attempt stored them.
        uploaded
            .retain(|path, _| path.starts_with("scenes/") || RUN_METADATA.contains(&path.as_str()));
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

        if !self
            .inner
            .jobs
            .set_runner_state(
                &job.id,
                &self.inner.settings.runner_id,
                JobState::Finalizing,
                None,
            )
            .await
            .map_err(|error| error.to_string())?
        {
            return Ok(Outcome::Stopped);
        }
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
            .set_progress(
                &job.id,
                &self.inner.settings.runner_id,
                job.committed_scenes,
            )
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
    if tree.is_file() {
        let size = std::fs::metadata(tree)
            .map_err(|error| format!("cannot stat {}: {error}", tree.display()))?
            .len();
        return Ok(vec![(relative_path(root, tree), size)]);
    }
    if !tree.exists() {
        return Ok(Vec::new());
    }
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
                files.push((relative_path(root, &path), size));
            }
        }
    }
    files.sort();
    Ok(files)
}

/// `path` below `root` as a `/`-separated relative path.
fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .expect("listed below the root")
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The visible directories directly below `directory`, as name, path, and
/// modification time in Unix milliseconds; empty when it cannot be read.
async fn local_entries(directory: &Path) -> Vec<(String, PathBuf, i64)> {
    let mut found = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(directory).await else {
        return found;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(metadata) = entry.metadata().await else {
            continue;
        };
        if name.starts_with('.') || !metadata.is_dir() {
            continue;
        }
        let modified_ms = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
            });
        found.push((name, entry.path(), modified_ms));
    }
    found
}

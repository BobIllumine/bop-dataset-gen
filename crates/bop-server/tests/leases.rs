//! Several servers sharing one database: job leases, cancels through any
//! server, rebuilding a job's local copy from storage, and local cleanup.
//! Against `SQLite` and local storage, without rendering; set
//! `BOP_TEST_DATABASE_URL` to an empty `PostgreSQL` database to run one test
//! (they share it) against `PostgreSQL`.

use std::time::Duration;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use bop_server::db::{JobState, JobStore, NewJob};
use serde_json::Value;
use tower::ServiceExt as _;

const LEASE_MS: i64 = 60_000;

fn database_url(root: &std::path::Path) -> String {
    std::env::var("BOP_TEST_DATABASE_URL")
        .unwrap_or_else(|_| format!("sqlite://{}?mode=rwc", root.join("bop.db").display()))
}

fn storage(root: &std::path::Path) -> bop_server::storage::Storage {
    bop_server::storage::Storage::from_url(&format!("file://{}/storage", root.display())).unwrap()
}

/// Runs one statement against the database, as a stalled or crashed server
/// would have left it.
async fn execute(root: &std::path::Path, sql: &str) {
    sqlx::any::install_default_drivers();
    let pool = sqlx::AnyPool::connect(&database_url(root)).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

async fn queued(jobs: &JobStore, id: &str) {
    jobs.insert(&NewJob {
        id,
        renderer: "wgpu",
        export_bop: false,
        config_toml: "",
        total_scenes: 4,
    })
    .await
    .unwrap();
}

fn settings(root: &std::path::Path, extra: &[(&str, &str)]) -> bop_server::settings::Settings {
    let database = database_url(root);
    let root = root.display().to_string();
    let extra = extra
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect::<Vec<_>>();
    bop_server::settings::Settings::from_vars(move |name| match name {
        "BOP_DATA_DIR" => Some(root.clone()),
        "BOP_DATABASE_URL" => Some(database.clone()),
        _ => extra
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone()),
    })
    .unwrap()
}

async fn send(app: &Router, method: &str, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn only_the_lease_holder_renews_and_records_and_expired_leases_are_released() {
    let root = tempfile::tempdir().unwrap();
    let jobs = JobStore::connect(&database_url(root.path())).await.unwrap();
    queued(&jobs, "a").await;
    let (job, _) = jobs
        .claim_next("server-1", LEASE_MS)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((job.id.as_str(), job.state), ("a", JobState::Running));
    assert!(
        jobs.claim_next("server-2", LEASE_MS)
            .await
            .unwrap()
            .is_none()
    );

    assert_eq!(
        jobs.renew_lease("a", "server-1", LEASE_MS).await.unwrap(),
        Some(false)
    );
    assert_eq!(
        jobs.renew_lease("a", "server-2", LEASE_MS).await.unwrap(),
        None
    );
    assert!(jobs.is_held_by("a", "server-1").await.unwrap());
    // A live lease is not released by others.
    assert!(jobs.release_interrupted(None).await.unwrap().is_empty());

    // The holder stalls past its lease.
    execute(root.path(), "UPDATE bop_jobs SET lease_expires_at_ms = 0").await;
    assert_eq!(jobs.release_interrupted(None).await.unwrap(), ["a"]);
    let job = jobs.get("a").await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Queued);
    assert_eq!(
        jobs.renew_lease("a", "server-1", LEASE_MS).await.unwrap(),
        None
    );
    assert!(
        !jobs
            .set_runner_state("a", "server-1", JobState::Failed, Some("late"))
            .await
            .unwrap(),
        "a server that lost the lease must not record a state"
    );
    assert_eq!(
        jobs.get("a").await.unwrap().unwrap().state,
        JobState::Queued
    );

    // Another server continues it, and its records stick.
    jobs.claim_next("server-2", LEASE_MS)
        .await
        .unwrap()
        .unwrap();
    assert!(
        jobs.set_runner_state("a", "server-2", JobState::Succeeded, None)
            .await
            .unwrap()
    );
    let job = jobs.get("a").await.unwrap().unwrap();
    assert_eq!(job.state, JobState::Succeeded);
    assert!(job.finished_at_ms.is_some());
}

#[tokio::test]
async fn a_restarted_server_releases_its_own_jobs_and_pending_cancels_win() {
    let root = tempfile::tempdir().unwrap();
    let jobs = JobStore::connect(&database_url(root.path())).await.unwrap();
    for id in ["mine", "theirs", "cancelled"] {
        queued(&jobs, id).await;
    }
    jobs.claim_next("me", LEASE_MS).await.unwrap().unwrap();
    jobs.claim_next("them", LEASE_MS).await.unwrap().unwrap();
    jobs.claim_next("them", LEASE_MS).await.unwrap().unwrap();
    assert!(jobs.request_cancel("cancelled").await.unwrap());
    assert_eq!(
        jobs.renew_lease("cancelled", "them", LEASE_MS)
            .await
            .unwrap(),
        Some(true)
    );

    assert_eq!(
        jobs.release_interrupted(Some("me")).await.unwrap(),
        ["mine"]
    );
    execute(
        root.path(),
        "UPDATE bop_jobs SET lease_expires_at_ms = 0 WHERE id = 'cancelled'",
    )
    .await;
    assert_eq!(jobs.release_interrupted(None).await.unwrap(), ["cancelled"]);
    let states = [
        ("mine", JobState::Queued),
        ("theirs", JobState::Running),
        ("cancelled", JobState::Cancelled),
    ];
    for (id, state) in states {
        let job = jobs.get(id).await.unwrap().unwrap();
        assert_eq!(job.state, state, "{id}");
        assert!(!job.cancel_requested, "{id}");
    }
}

#[tokio::test]
async fn any_server_can_cancel_a_job_another_one_runs() {
    let root = tempfile::tempdir().unwrap();
    let (app, _runner) = bop_server::build(settings(root.path(), &[])).await.unwrap();
    let jobs = JobStore::connect(&database_url(root.path())).await.unwrap();
    queued(&jobs, "elsewhere").await;
    jobs.claim_next("other-server", LEASE_MS)
        .await
        .unwrap()
        .unwrap();

    let (status, _) = send(&app, "POST", "/api/v1/jobs/elsewhere/cancel").await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (_, job) = send(&app, "GET", "/api/v1/jobs/elsewhere").await;
    assert_eq!(job["state"], "running");
    assert_eq!(job["cancel_requested"], true);
    // The other server learns of it at its next renewal.
    assert_eq!(
        jobs.renew_lease("elsewhere", "other-server", LEASE_MS)
            .await
            .unwrap(),
        Some(true)
    );

    jobs.set_runner_state("elsewhere", "other-server", JobState::Cancelled, None)
        .await
        .unwrap();
    let (status, _) = send(&app, "POST", "/api/v1/jobs/elsewhere/cancel").await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_local_copy_is_rebuilt_from_the_complete_scenes_in_storage() {
    let root = tempfile::tempdir().unwrap();
    let jobs = JobStore::connect(&database_url(root.path())).await.unwrap();
    let store = storage(root.path());
    queued(&jobs, "job").await;
    let checksums = |files: &[&str]| {
        let files = files
            .iter()
            .map(|file| ((*file).to_owned(), Value::from("0")))
            .collect::<serde_json::Map<_, _>>();
        serde_json::json!({ "schema_version": 1, "files": files }).to_string()
    };
    let stored = [
        ("run.json", "{}".to_owned()),
        ("environment.json", "{\"env\":1}".to_owned()),
        (
            "scenes/00/checksums.json",
            checksums(&["scene.json", "frames/1/rgb.png"]),
        ),
        ("scenes/00/scene.json", "{}".to_owned()),
        ("scenes/00/frames/1/rgb.png", "png".to_owned()),
        // Interrupted while uploading: its frame never arrived.
        (
            "scenes/01/checksums.json",
            checksums(&["scene.json", "frames/1/rgb.png"]),
        ),
        ("scenes/01/scene.json", "{}".to_owned()),
        // Present locally already, so left alone.
        ("scenes/02/checksums.json", checksums(&["scene.json"])),
        ("scenes/02/scene.json", "{\"stored\":true}".to_owned()),
    ];
    let mut index = Vec::new();
    for (path, body) in &stored {
        store
            .put(&format!("datasets/job/{path}"), body.as_bytes().to_vec())
            .await
            .unwrap();
        index.push(((*path).to_owned(), body.len() as u64));
    }
    jobs.record_job_files("job", &index).await.unwrap();
    let dataset = root.path().join("dataset");
    std::fs::create_dir_all(dataset.join("scenes/02")).unwrap();
    std::fs::write(dataset.join("scenes/02/scene.json"), "local").unwrap();

    let rebuilt = bop_server::index::rebuild_local_copy(&jobs, &store, "job", &dataset)
        .await
        .unwrap();
    assert_eq!(rebuilt, 1);
    let read = |path: &str| std::fs::read_to_string(dataset.join(path)).unwrap();
    assert_eq!(read("scenes/00/frames/1/rgb.png"), "png");
    assert!(dataset.join("scenes/00/checksums.json").is_file());
    assert!(!dataset.join("scenes/01").exists());
    assert_eq!(read("scenes/02/scene.json"), "local");
    assert_eq!(read("run.json"), "{}");
    assert_eq!(read("environment.json"), "{\"env\":1}");
    assert!(!dataset.join(".rebuild").exists());
}

#[tokio::test]
async fn old_local_copies_and_stale_cached_assets_are_deleted() {
    let root = tempfile::tempdir().unwrap();
    let (app, runner) =
        bop_server::build(settings(root.path(), &[("BOP_LOCAL_RETENTION_DAYS", "1")]))
            .await
            .unwrap();
    let request = Request::builder()
        .method("PUT")
        .uri("/api/v1/assets/cup.glb")
        .body(Body::from("cup"))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::CREATED
    );
    let jobs = JobStore::connect(&database_url(root.path())).await.unwrap();
    for id in ["old-failure", "new-failure", "old-success"] {
        queued(&jobs, id).await;
        std::fs::create_dir_all(root.path().join("jobs").join(id).join("dataset")).unwrap();
    }
    jobs.set_state("old-failure", JobState::Failed, Some("boom"))
        .await
        .unwrap();
    jobs.set_state("new-failure", JobState::Failed, Some("boom"))
        .await
        .unwrap();
    jobs.set_state("old-success", JobState::Succeeded, None)
        .await
        .unwrap();
    execute(
        root.path(),
        "UPDATE bop_jobs SET updated_at_ms = 0 WHERE id IN ('old-failure', 'old-success')",
    )
    .await;
    let etag = jobs.asset("cup.glb").await.unwrap().unwrap().etag.unwrap();
    let cache = root.path().join("asset-cache");
    let current = cache
        .join(format!("{}-3", etag.trim_matches('"')))
        .join("cup.glb");
    let stale = cache.join("replaced-9").join("cup.glb");
    for file in [&current, &stale] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, "cup").unwrap();
    }

    let running = tokio::spawn(runner.clone().run());
    let jobs_dir = root.path().join("jobs");
    tokio::time::timeout(Duration::from_secs(10), async {
        while jobs_dir.join("old-failure").exists() || stale.exists() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the sweep runs at start");
    runner.shutdown();
    running.await.unwrap();
    assert!(jobs_dir.join("new-failure").exists());
    assert!(jobs_dir.join("old-success").exists());
    assert!(current.exists(), "{}", current.display());
    assert!(!stale.parent().unwrap().exists());
}

#[tokio::test]
async fn job_changes_reach_other_servers_subscribers_on_postgresql() {
    let root = tempfile::tempdir().unwrap();
    if !database_url(root.path()).starts_with("postgres") {
        return;
    }
    let (_app, runner) = bop_server::build(settings(root.path(), &[])).await.unwrap();
    let mut events = runner.subscribe();
    let running = tokio::spawn(runner.clone().run());
    // Another server's store; give the listener a moment to subscribe.
    let other = JobStore::connect(&database_url(root.path())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    queued(&other, "from-elsewhere").await;
    let event = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.job_id == "from-elsewhere" {
                return event;
            }
        }
    })
    .await
    .expect("the change is announced here");
    // Heard from the other store, before this server claimed the job.
    assert_eq!(event.state, JobState::Queued);
    assert_eq!(event.total_scenes, 4);
    runner.shutdown();
    running.await.unwrap();
}

//! A real job through the API, runner, renderer, database, and storage.
//!
//! Uses `BOP_TEST_DATABASE_URL` and `BOP_TEST_STORAGE_URL` when set (S3
//! connection from the usual `AWS_*` variables), else `SQLite` and a local
//! directory. Needs a wgpu adapter; lavapipe is enough.

use std::time::Duration;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::Value;
use tower::ServiceExt as _;

const SMOKE: &str = include_str!("../../../examples/presets/smoke.toml");
const CUBE: &[u8] = include_bytes!("../../../examples/assets/cube.stl");

async fn send(app: &Router, method: &str, uri: &str, body: Body) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires a wgpu adapter; set BOP_TEST_DATABASE_URL and BOP_TEST_STORAGE_URL for PostgreSQL and S3"]
async fn a_submitted_job_is_rendered_validated_exported_and_stored() {
    let root = tempfile::tempdir().unwrap();
    let data_dir = root.path().display().to_string();
    let settings = bop_server::settings::Settings::from_vars(|name| match name {
        "BOP_DATA_DIR" => Some(data_dir.clone()),
        "BOP_DATABASE_URL" => std::env::var("BOP_TEST_DATABASE_URL").ok(),
        "BOP_STORAGE_URL" => std::env::var("BOP_TEST_STORAGE_URL").ok(),
        _ => None,
    })
    .unwrap();
    let (app, runner) = bop_server::build(settings).await.unwrap();
    let running = tokio::spawn(runner.clone().run());

    let (status, _) = send(&app, "PUT", "/api/v1/assets/cube.stl", Body::from(CUBE)).await;
    assert_eq!(status, StatusCode::CREATED);
    let config = SMOKE.replace("\"../assets/cube.stl\"", "\"cube.stl\"");
    assert_ne!(config, SMOKE, "the smoke preset still names its cube");
    let request = serde_json::json!({ "config": config, "renderer": "wgpu", "export_bop": true });
    let (status, created) = send(
        &app,
        "POST",
        "/api/v1/jobs",
        Body::from(request.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();

    let job = tokio::time::timeout(Duration::from_mins(10), async {
        loop {
            let (_, job) = send(&app, "GET", &format!("/api/v1/jobs/{id}"), Body::empty()).await;
            if ["succeeded", "failed", "cancelled"].contains(&job["state"].as_str().unwrap()) {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
    .await
    .expect("job finishes within ten minutes");
    assert_eq!(job["state"], "succeeded", "{job}");
    assert_eq!(job["committed_scenes"], 8);

    let (_, files) = send(
        &app,
        "GET",
        &format!("/api/v1/jobs/{id}/files"),
        Body::empty(),
    )
    .await;
    let paths = files
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect::<Vec<_>>();
    for expected in ["run.json", "resolved-config.toml"] {
        assert!(
            paths.contains(&expected),
            "{expected} missing from {paths:?}"
        );
    }
    // The BOP export is stored only as its archive.
    assert!(
        !paths.iter().any(|path| path.starts_with("bop/")),
        "{paths:?}"
    );
    let frames = paths
        .iter()
        .filter(|path| path.ends_with("/rgb.png"))
        .count();
    assert!(frames >= 8, "expected a frame per scene, found {frames}");
    let first = paths
        .iter()
        .find(|path| path.ends_with("/rgb.png"))
        .unwrap();
    let request = Request::builder()
        .uri(format!("/api/v1/jobs/{id}/files/{first}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let png = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert!(png.starts_with(b"\x89PNG"));

    assert_exports_ready(&app, &root.path().join("bop.db"), &id).await;

    runner.shutdown();
    running.await.unwrap();
}

/// Both archives were packed as the job finished; their links work at once.
async fn assert_exports_ready(app: &Router, sqlite: &std::path::Path, id: &str) {
    for (format, inside) in [("dataset", "run.json"), ("bop", "camera.json")] {
        let jobs = bop_server::db::JobStore::connect(
            &std::env::var("BOP_TEST_DATABASE_URL")
                .unwrap_or_else(|_| format!("sqlite://{}?mode=rwc", sqlite.display())),
        )
        .await
        .unwrap();
        assert!(
            matches!(
                jobs.export(id, format).await.unwrap(),
                Some(bop_server::db::ExportRecord::Ready { .. })
            ),
            "{format} export is not ready"
        );
        let body = serde_json::json!({ "format": format }).to_string();
        let (status, link) = send(
            app,
            "POST",
            &format!("/api/v1/jobs/{id}/exports"),
            Body::from(body),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{link}");
        if link["signed"] == false {
            let request = Request::builder()
                .uri(link["url"].as_str().unwrap())
                .body(Body::empty())
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            let archive = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            assert_eq!(link["size"], archive.len());
            let names = tar::Archive::new(archive.as_ref())
                .entries()
                .unwrap()
                .map(|entry| entry.unwrap().path().unwrap().display().to_string())
                .collect::<Vec<_>>();
            let expected = format!("{format}-{id}/{inside}");
            assert!(
                names.contains(&expected),
                "{expected} missing from {names:?}"
            );
        }
    }
}

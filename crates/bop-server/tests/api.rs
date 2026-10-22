//! API behaviour against `SQLite` and local storage, without running jobs.

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::Value;
use tower::ServiceExt as _;

const CONFIG: &str = include_str!("../../../examples/cup_nippel/cup_nippel.toml");

async fn app(root: &std::path::Path, token: Option<&str>) -> Router {
    app_with(root, token, None).await
}

async fn app_with(root: &std::path::Path, token: Option<&str>, cors: Option<&str>) -> Router {
    let root = root.to_owned();
    let token = token.map(str::to_owned);
    let cors = cors.map(str::to_owned);
    let settings = bop_server::settings::Settings::from_vars(move |name| match name {
        "BOP_DATA_DIR" => Some(root.display().to_string()),
        "BOP_API_TOKEN" => token.clone(),
        "BOP_MAX_SCENES" => Some("1000".to_owned()),
        "BOP_CORS_ORIGINS" => cors.clone(),
        // These tests manage the library themselves; seeding has its own.
        "BOP_SEED_EXAMPLES" => Some("false".to_owned()),
        _ => None,
    })
    .unwrap();
    bop_server::build(settings).await.unwrap().0
}

/// Sends a bodiless request and returns the status, headers, and raw body.
async fn fetch(app: &Router, method: &str, uri: &str) -> (StatusCode, header::HeaderMap, Vec<u8>) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    (parts.status, parts.headers, bytes.to_vec())
}

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

/// A submission that leaves `export_bop` to its default (true).
fn job(config: &str) -> Body {
    Body::from(serde_json::json!({ "config": config, "renderer": "wgpu" }).to_string())
}

#[tokio::test]
async fn jobs_need_their_assets_and_move_through_cancel_and_resume() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), None).await;

    let (status, body) = send(&app, "POST", "/api/v1/jobs", job(CONFIG)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("has not been uploaded")
    );

    for asset in ["cup/cup.glb", "nippel/nippel.glb"] {
        let (status, _) = send(
            &app,
            "PUT",
            &format!("/api/v1/assets/{asset}"),
            Body::from(vec![0_u8; 16]),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (_, assets) = send(&app, "GET", "/api/v1/assets", Body::empty()).await;
    assert_eq!(assets[0]["path"], "cup/cup.glb");
    assert_eq!(assets[1]["size"], 16);

    let (status, created) = send(&app, "POST", "/api/v1/jobs", job(CONFIG)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["state"], "queued");
    assert_eq!(created["total_scenes"], 400);
    assert_eq!(created["export_bop"], true);
    assert_eq!(created["cancel_requested"], false);
    let id = created["id"].as_str().unwrap();

    let (status, listed) = send(&app, "GET", "/api/v1/jobs", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed[0]["id"], id);

    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/jobs/{id}/cancel"),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, cancelled) = send(&app, "GET", &format!("/api/v1/jobs/{id}"), Body::empty()).await;
    assert_eq!(cancelled["state"], "cancelled");
    assert!(cancelled["finished_at_ms"].is_i64());

    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/jobs/{id}/resume"),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (_, resumed) = send(&app, "GET", &format!("/api/v1/jobs/{id}"), Body::empty()).await;
    assert_eq!(resumed["state"], "queued");
    let (status, _) = send(
        &app,
        "POST",
        &format!("/api/v1/jobs/{id}/resume"),
        Body::empty(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, _) = send(&app, "GET", "/api/v1/jobs/missing", Body::empty()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn submissions_cannot_escape_the_asset_library_or_exceed_limits() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), None).await;

    let (status, _) = send(
        &app,
        "PUT",
        "/api/v1/assets/..%2Fescape.glb",
        Body::from("x"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let absolute = CONFIG.replace("\"cup/cup.glb\"", "\"/etc/cup.glb\"");
    let (status, body) = send(&app, "POST", "/api/v1/jobs", job(&absolute)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"].as_str().unwrap().contains("relative"));

    let huge = CONFIG.replace("valid_scene_count = 400", "valid_scene_count = 5000");
    let (status, body) = send(&app, "POST", "/api/v1/jobs", job(&huge)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"].as_str().unwrap().contains("limit of 1000"));

    let (status, _) = send(
        &app,
        "POST",
        "/api/v1/jobs",
        Body::from(serde_json::json!({ "config": CONFIG, "renderer": "povray" }).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn a_configured_token_guards_the_api_but_not_the_health_check() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), Some("secret")).await;
    let (status, _) = send(&app, "GET", "/api/v1/jobs", Body::empty()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let request = Request::builder()
        .uri("/api/v1/jobs")
        .header(header::AUTHORIZATION, "Bearer secret")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::OK
    );
    let health = Request::builder()
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();
    assert_eq!(app.oneshot(health).await.unwrap().status(), StatusCode::OK);
}

#[tokio::test]
async fn version_names_the_api_and_the_compiled_renderers() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), None).await;
    let (status, info) = send(&app, "GET", "/api/v1/version", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["api"], "v1");
    assert_eq!(info["server"], env!("CARGO_PKG_VERSION"));
    assert_eq!(info["max_scenes"], 1000);
    assert_eq!(info["signed_exports"], false);
    assert!(
        info["renderers"]
            .as_array()
            .unwrap()
            .contains(&Value::from("wgpu"))
    );
}

#[tokio::test]
async fn assets_download_and_configs_validate_without_queuing_a_job() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), None).await;
    let validate = |config: &str| Body::from(serde_json::json!({ "config": config }).to_string());

    let (status, report) = send(&app, "POST", "/api/v1/configs/validate", validate(CONFIG)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(report["total_scenes"], 400);
    assert_eq!(
        report["missing_assets"],
        serde_json::json!(["cup/cup.glb", "nippel/nippel.glb"])
    );

    for asset in ["cup/cup.glb", "nippel/nippel.glb"] {
        send(
            &app,
            "PUT",
            &format!("/api/v1/assets/{asset}"),
            Body::from(asset.as_bytes().to_vec()),
        )
        .await;
    }
    let (_, report) = send(&app, "POST", "/api/v1/configs/validate", validate(CONFIG)).await;
    assert_eq!(report["missing_assets"], serde_json::json!([]));
    let (_, jobs) = send(&app, "GET", "/api/v1/jobs", Body::empty()).await;
    assert_eq!(jobs, serde_json::json!([]));

    let (status, headers, body) = fetch(&app, "GET", "/api/v1/assets/cup/cup.glb").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "model/gltf-binary");
    assert_eq!(body, b"cup/cup.glb");
    let (status, _, _) = fetch(&app, "GET", "/api/v1/assets/cup/missing.glb").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = send(
        &app,
        "POST",
        "/api/v1/configs/validate",
        validate("[run]\nmaster_seed = 1\n"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .starts_with("invalid config")
    );
}

#[tokio::test]
async fn succeeded_jobs_export_as_tar_archives_through_the_api_on_local_storage() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), None).await;
    for asset in ["cup/cup.glb", "nippel/nippel.glb"] {
        send(
            &app,
            "PUT",
            &format!("/api/v1/assets/{asset}"),
            Body::from(vec![0_u8; 4]),
        )
        .await;
    }
    let (_, created) = send(&app, "POST", "/api/v1/jobs", job(CONFIG)).await;
    let id = created["id"].as_str().unwrap().to_owned();
    let export = |format: &str| {
        Body::from(serde_json::json!({ "format": format, "expires_in_s": 3600 }).to_string())
    };
    let exports = format!("/api/v1/jobs/{id}/exports");

    let (status, _) = send(&app, "POST", &exports, export("dataset")).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Stand in for the runner: store a finished dataset and mark the job done.
    let storage = bop_server::storage::Storage::from_url(&format!(
        "file://{}/storage",
        root.path().display()
    ))
    .unwrap();
    for (path, body) in [
        ("manifest.json", "{}"),
        ("scenes/000000/scene.json", "{\"objects\":[]}"),
        ("bop/scene_gt.json", "[]"),
    ] {
        storage
            .put(&format!("datasets/{id}/{path}"), body.as_bytes().to_vec())
            .await
            .unwrap();
    }
    let jobs = bop_server::db::JobStore::connect(&format!(
        "sqlite://{}?mode=rwc",
        root.path().join("bop.db").display()
    ))
    .await
    .unwrap();
    jobs.set_state(&id, bop_server::db::JobState::Succeeded, None)
        .await
        .unwrap();

    let (status, body) = send(
        &app,
        "POST",
        &exports,
        Body::from(serde_json::json!({ "format": "dataset", "expires_in_s": 5 }).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("expires_in_s"));

    for (format, expected) in [
        (
            "dataset",
            vec![
                format!("dataset-{id}/manifest.json"),
                format!("dataset-{id}/scenes/000000/scene.json"),
            ],
        ),
        ("bop", vec![format!("bop-{id}/scene_gt.json")]),
    ] {
        let (status, link) = send(&app, "POST", &exports, export(format)).await;
        assert_eq!(status, StatusCode::OK, "{link}");
        assert_eq!(link["signed"], false);
        assert_eq!(link["expires_at_ms"], Value::Null);
        assert_eq!(link["file_name"], format!("{format}-{id}.tar"));
        let url = link["url"].as_str().unwrap();
        assert_eq!(url, format!("/api/v1/jobs/{id}/exports/{format}.tar"));

        let (status, headers, archive) = fetch(&app, "GET", url).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers[header::CONTENT_TYPE], "application/x-tar");
        assert_eq!(link["size"], archive.len());
        let names = tar::Archive::new(archive.as_slice())
            .entries()
            .unwrap()
            .map(|entry| entry.unwrap().path().unwrap().display().to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, expected);
    }

    let (status, _, _) = fetch(&app, "GET", &format!("/api/v1/jobs/{id}/exports/other.tar")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cors_is_off_by_default_and_allows_configured_origins() {
    let root = tempfile::tempdir().unwrap();
    let preflight = |app: Router| async move {
        let request = Request::builder()
            .method("OPTIONS")
            .uri("/api/v1/jobs")
            .header(header::ORIGIN, "https://studio.example")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(header::ACCESS_CONTROL_REQUEST_HEADERS, "authorization")
            .body(Body::empty())
            .unwrap();
        app.oneshot(request).await.unwrap()
    };

    let closed = preflight(app(root.path(), None).await).await;
    assert!(
        !closed
            .headers()
            .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
    );

    let open =
        preflight(app_with(root.path(), Some("secret"), Some("https://studio.example")).await)
            .await;
    assert_eq!(open.status(), StatusCode::OK);
    assert_eq!(
        open.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://studio.example"
    );
}

#[tokio::test]
async fn optimizing_an_asset_writes_a_sibling_glb_to_the_library() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), None).await;
    let cube = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/assets/cube.stl"),
    )
    .unwrap();
    let (status, _) = send(
        &app,
        "PUT",
        "/api/v1/assets/parts/cube.stl",
        Body::from(cube),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let optimize = |body: serde_json::Value| Body::from(body.to_string());

    let (status, body) = send(
        &app,
        "POST",
        "/api/v1/asset-optimizations",
        optimize(serde_json::json!({ "path": "parts/cube.stl" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("source_unit_scale_m")
    );

    let (status, report) = send(
        &app,
        "POST",
        "/api/v1/asset-optimizations",
        optimize(serde_json::json!({ "path": "parts/cube.stl", "source_unit_scale_m": 0.001 })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{report}");
    assert_eq!(report["path"], "parts/cube.optimized.glb");
    assert_eq!(report["source_triangles"], 12);
    assert_eq!(report["triangles"], 12);
    assert_eq!(report["vertices"], 24);
    assert!(report["max_deviation_mm"].as_f64().unwrap() <= 0.1);

    let (_, assets) = send(&app, "GET", "/api/v1/assets", Body::empty()).await;
    let paths: Vec<&str> = assets
        .as_array()
        .unwrap()
        .iter()
        .map(|asset| asset["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["parts/cube.optimized.glb", "parts/cube.stl"]);
    assert_eq!(assets[0]["size"], report["bytes"]);

    for (path, expected) in [
        ("parts/cube.optimized.glb", StatusCode::BAD_REQUEST),
        ("parts/missing.glb", StatusCode::NOT_FOUND),
        ("../escape.glb", StatusCode::BAD_REQUEST),
    ] {
        let (status, _) = send(
            &app,
            "POST",
            "/api/v1/asset-optimizations",
            optimize(serde_json::json!({ "path": path })),
        )
        .await;
        assert_eq!(status, expected, "{path}");
    }
}

#[tokio::test]
async fn environment_maps_are_library_assets() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), None).await;
    let with_environment = |path: &str| {
        let config = CONFIG.replace(
            "material_defaults =",
            &format!("environment = {{ path = \"{path}\" }}\nmaterial_defaults ="),
        );
        Body::from(serde_json::json!({ "config": config }).to_string())
    };
    let (status, report) = send(
        &app,
        "POST",
        "/api/v1/configs/validate",
        with_environment("skies/studio.hdr"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(
        report["missing_assets"],
        serde_json::json!(["cup/cup.glb", "nippel/nippel.glb", "skies/studio.hdr"])
    );

    let (status, _) = send(
        &app,
        "POST",
        "/api/v1/configs/validate",
        with_environment("../outside.hdr"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

fn storage(root: &std::path::Path) -> bop_server::storage::Storage {
    bop_server::storage::Storage::from_url(&format!("file://{}/storage", root.display())).unwrap()
}

fn database_url(root: &std::path::Path) -> String {
    format!("sqlite://{}?mode=rwc", root.join("bop.db").display())
}

async fn jobs(root: &std::path::Path) -> bop_server::db::JobStore {
    bop_server::db::JobStore::connect(&database_url(root))
        .await
        .unwrap()
}

/// Runs one statement against the server's database, as an older server or
/// a crashed one would have left it.
async fn execute(root: &std::path::Path, sql: &str) {
    sqlx::any::install_default_drivers();
    let pool = sqlx::AnyPool::connect(&database_url(root)).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

/// Queues a job for `CONFIG` and returns its identifier.
async fn queued_job(app: &Router) -> String {
    for asset in ["cup/cup.glb", "nippel/nippel.glb"] {
        send(
            app,
            "PUT",
            &format!("/api/v1/assets/{asset}"),
            Body::from(vec![0_u8; 4]),
        )
        .await;
    }
    let (status, created) = send(app, "POST", "/api/v1/jobs", job(CONFIG)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    created["id"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn the_asset_index_is_rebuilt_from_storage_at_every_start() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), None).await;
    send(&app, "PUT", "/api/v1/assets/cup/cup.glb", Body::from("cup")).await;
    // Changed behind the server's back: one added, one removed.
    let store = storage(root.path());
    store
        .put("assets/nippel/nippel.glb", b"nippel!".to_vec())
        .await
        .unwrap();
    store.delete("assets/cup/cup.glb").await.unwrap();
    let (_, assets) = send(&app, "GET", "/api/v1/assets", Body::empty()).await;
    assert_eq!(
        assets,
        serde_json::json!([{ "path": "cup/cup.glb", "size": 3 }])
    );

    let app = self::app(root.path(), None).await;
    let (_, assets) = send(&app, "GET", "/api/v1/assets", Body::empty()).await;
    assert_eq!(
        assets,
        serde_json::json!([{ "path": "nippel/nippel.glb", "size": 7 }])
    );
    let validate = Body::from(serde_json::json!({ "config": CONFIG }).to_string());
    let (_, report) = send(&app, "POST", "/api/v1/configs/validate", validate).await;
    assert_eq!(report["missing_assets"], serde_json::json!(["cup/cup.glb"]));

    let (status, _, _) = fetch(&app, "DELETE", "/api/v1/assets/nippel/nippel.glb").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, assets) = send(&app, "GET", "/api/v1/assets", Body::empty()).await;
    assert_eq!(assets, serde_json::json!([]));
}

#[tokio::test]
async fn files_of_jobs_stored_before_the_index_are_indexed_on_first_listing() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), None).await;
    let id = queued_job(&app).await;
    let store = storage(root.path());
    for path in ["scenes/1/scene.json", "run.json", "scenes/0/scene.json"] {
        store
            .put(&format!("datasets/{id}/{path}"), b"{}".to_vec())
            .await
            .unwrap();
    }
    let files = format!("/api/v1/jobs/{id}/files");
    // A new job's listing comes from the index, which these writes bypassed.
    let (_, listed) = send(&app, "GET", &files, Body::empty()).await;
    assert_eq!(listed, serde_json::json!([]));

    execute(root.path(), "UPDATE bop_jobs SET files_indexed = 0").await;
    let (status, listed) = send(&app, "GET", &files, Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        listed,
        serde_json::json!([
            { "path": "run.json", "size": 2 },
            { "path": "scenes/0/scene.json", "size": 2 },
            { "path": "scenes/1/scene.json", "size": 2 },
        ])
    );
    assert_eq!(
        jobs(root.path()).await.files_indexed(&id).await.unwrap(),
        Some(true)
    );

    let (status, listed) = send(&app, "GET", "/api/v1/jobs/unknown/files", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed, serde_json::json!([]));
}

#[tokio::test]
async fn export_requests_wait_for_the_one_packing_and_reuse_its_archive() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), None).await;
    let id = queued_job(&app).await;
    let jobs = jobs(root.path()).await;
    jobs.set_state(&id, bop_server::db::JobState::Succeeded, None)
        .await
        .unwrap();
    // Another request (or server) is packing the dataset export.
    assert!(jobs.claim_export(&id, "dataset").await.unwrap());
    assert!(!jobs.claim_export(&id, "dataset").await.unwrap());

    let request = {
        let (app, id) = (app.clone(), id.clone());
        tokio::spawn(async move {
            let body = serde_json::json!({ "format": "dataset" }).to_string();
            send(
                &app,
                "POST",
                &format!("/api/v1/jobs/{id}/exports"),
                Body::from(body),
            )
            .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    assert!(!request.is_finished(), "the request waits for the packer");
    storage(root.path())
        .put(&format!("exports/{id}/dataset.tar"), vec![0; 1024])
        .await
        .unwrap();
    jobs.set_export_ready(&id, "dataset", 1024).await.unwrap();
    let (status, link) = request.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{link}");
    assert_eq!(link["size"], 1024);
}

#[tokio::test]
async fn an_abandoned_export_claim_is_taken_over() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), None).await;
    let id = queued_job(&app).await;
    let jobs = jobs(root.path()).await;
    storage(root.path())
        .put(&format!("datasets/{id}/run.json"), b"{}".to_vec())
        .await
        .unwrap();
    jobs.set_state(&id, bop_server::db::JobState::Succeeded, None)
        .await
        .unwrap();
    // A packer that crashed long ago.
    assert!(jobs.claim_export(&id, "dataset").await.unwrap());
    execute(root.path(), "UPDATE bop_exports SET updated_at_ms = 0").await;
    assert!(!jobs.take_over_export(&id, "dataset", 1).await.unwrap());

    let body = serde_json::json!({ "format": "dataset" }).to_string();
    let (status, link) = send(
        &app,
        "POST",
        &format!("/api/v1/jobs/{id}/exports"),
        Body::from(body),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{link}");
    let (_, _, archive) = fetch(&app, "GET", link["url"].as_str().unwrap()).await;
    assert_eq!(link["size"], archive.len());
    assert_eq!(
        jobs.export(&id, "dataset").await.unwrap(),
        Some(bop_server::db::ExportRecord::Ready {
            size: archive.len() as u64
        })
    );
}

/// A server on `root` with example seeding at its default.
async fn seeding_app(root: &std::path::Path) -> Router {
    let root = root.to_owned();
    let settings = bop_server::settings::Settings::from_vars(move |name| match name {
        "BOP_DATA_DIR" => Some(root.display().to_string()),
        _ => None,
    })
    .unwrap();
    bop_server::build(settings).await.unwrap().0
}

async fn asset_paths(app: &Router) -> Vec<String> {
    let (_, assets) = send(app, "GET", "/api/v1/assets", Body::empty()).await;
    assets
        .as_array()
        .unwrap()
        .iter()
        .map(|asset| asset["path"].as_str().unwrap().to_owned())
        .collect()
}

const EXAMPLES: [&str; 4] = [
    "cube.stl",
    "hex_prism.stl",
    "l_bracket.stl",
    "textured_cube.glb",
];

#[tokio::test]
async fn a_new_library_starts_with_the_example_meshes_the_presets_use() {
    let root = tempfile::tempdir().unwrap();
    let app = seeding_app(root.path()).await;
    assert_eq!(asset_paths(&app).await, EXAMPLES);
    let (_, _, cube) = fetch(&app, "GET", "/api/v1/assets/cube.stl").await;
    assert_eq!(cube, include_bytes!("../../../examples/assets/cube.stl"));

    // The smoke preset, as the studio ships it, needs nothing uploaded.
    let smoke = include_str!("../../../examples/presets/smoke.toml")
        .replace("\"../assets/cube.stl\"", "\"cube.stl\"");
    let validate = Body::from(serde_json::json!({ "config": smoke }).to_string());
    let (status, report) = send(&app, "POST", "/api/v1/configs/validate", validate).await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["missing_assets"], serde_json::json!([]));
}

#[tokio::test]
async fn examples_never_replace_library_files_or_return_once_deleted() {
    let root = tempfile::tempdir().unwrap();
    let store = storage(root.path());
    store
        .put("assets/cube.stl", b"my own cube".to_vec())
        .await
        .unwrap();
    let app = seeding_app(root.path()).await;
    assert_eq!(asset_paths(&app).await, EXAMPLES);
    let (_, _, cube) = fetch(&app, "GET", "/api/v1/assets/cube.stl").await;
    assert_eq!(cube, b"my own cube");

    let (status, _, _) = fetch(&app, "DELETE", "/api/v1/assets/hex_prism.stl").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let app = seeding_app(root.path()).await;
    assert_eq!(
        asset_paths(&app).await,
        ["cube.stl", "l_bracket.stl", "textured_cube.glb"]
    );
}

#[tokio::test]
async fn seeding_can_be_turned_off() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), None).await;
    assert!(asset_paths(&app).await.is_empty());
    // A later start with seeding on still gives a never-seeded library the examples.
    let app = seeding_app(root.path()).await;
    assert_eq!(asset_paths(&app).await, EXAMPLES);
}

/// Sends a bodiless request with extra headers.
async fn fetch_with(
    app: &Router,
    uri: &str,
    headers: &[(header::HeaderName, &str)],
) -> (StatusCode, header::HeaderMap, Vec<u8>) {
    let mut request = Request::builder().uri(uri);
    for (name, value) in headers {
        request = request.header(name, *value);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    (parts.status, parts.headers, bytes.to_vec())
}

#[tokio::test]
async fn stored_files_are_cached_and_revalidated_by_entity_tag() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), None).await;
    let id = queued_job(&app).await;
    let store = storage(root.path());
    for path in ["scenes/1/scene.json", "run.json"] {
        store
            .put(&format!("datasets/{id}/{path}"), b"{}".to_vec())
            .await
            .unwrap();
    }

    // Committed scenes never change.
    let scene = format!("/api/v1/jobs/{id}/files/scenes/1/scene.json");
    let (status, headers, _) = fetch_with(&app, &scene, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers[header::CACHE_CONTROL],
        "private, max-age=31536000, immutable"
    );

    // Other files are revalidated: a matching tag costs no body.
    let run = format!("/api/v1/jobs/{id}/files/run.json");
    let (_, headers, body) = fetch_with(&app, &run, &[]).await;
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-cache");
    assert_eq!(body, b"{}");
    let etag = headers[header::ETAG].to_str().unwrap().to_owned();
    assert!(etag.starts_with('"') && etag.ends_with('"'), "{etag}");
    let (status, headers, body) = fetch_with(&app, &run, &[(header::IF_NONE_MATCH, &etag)]).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert_eq!(headers[header::ETAG], etag.as_str());
    assert!(body.is_empty());
    let (status, _, body) =
        fetch_with(&app, &run, &[(header::IF_NONE_MATCH, "\"something else\"")]).await;
    assert_eq!(
        (status, body.as_slice()),
        (StatusCode::OK, b"{}".as_slice())
    );

    // A replaced asset is noticed.
    let asset = "/api/v1/assets/cup/cup.glb";
    let (_, headers, _) = fetch_with(&app, asset, &[]).await;
    let old = headers[header::ETAG].to_str().unwrap().to_owned();
    assert_eq!(headers[header::CACHE_CONTROL], "private, no-cache");
    send(&app, "PUT", asset, Body::from(vec![1_u8; 9])).await;
    let (status, _, body) = fetch_with(&app, asset, &[(header::IF_NONE_MATCH, &old)]).await;
    assert_eq!((status, body.len()), (StatusCode::OK, 9));
}

#[tokio::test]
async fn frames_have_small_coloured_previews() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), None).await;
    let id = queued_job(&app).await;
    let store = storage(root.path());
    let frame = root.path().join("frame");
    std::fs::create_dir_all(&frame).unwrap();
    let (width, height) = (64_u32, 48_u32);
    let pixels = (width * height) as usize;
    bop_dataset::RgbFrame::try_new(width, height, vec![200; pixels * 3])
        .unwrap()
        .write_png(&frame.join("rgb.png"), bop_core::PngCompression::Fast)
        .unwrap();
    // Background on the left quarter, a slope elsewhere.
    let depth = (0..pixels)
        .map(|i| {
            if i % 64 < 16 {
                0.0
            } else {
                0.5 + 0.01 * f32::from(u8::try_from(i % 64).unwrap())
            }
        })
        .collect();
    bop_dataset::DepthFrame::try_new(width, height, depth)
        .unwrap()
        .write_exr(&frame.join("depth.exr"))
        .unwrap();
    // Instance 1 on the left half, background on the right.
    let ids = (0..pixels).map(|i| u16::from(i % 64 < 32)).collect();
    bop_dataset::InstanceFrame::try_new(width, height, ids)
        .unwrap()
        .write_png(&frame.join("instance.png"), bop_core::PngCompression::Fast)
        .unwrap();
    for name in ["rgb.png", "depth.exr", "instance.png"] {
        store
            .put_file(
                &format!("datasets/{id}/scenes/7/frames/00001/{name}"),
                &frame.join(name),
            )
            .await
            .unwrap();
    }
    let preview = |name: &str, query: &str| {
        format!("/api/v1/jobs/{id}/previews/scenes/7/frames/00001/{name}{query}")
    };

    for (name, content_type) in [
        ("rgb.png", "image/jpeg"),
        ("depth.exr", "image/jpeg"),
        ("instance.png", "image/png"),
    ] {
        let (status, headers, body) = fetch_with(&app, &preview(name, "?size=32"), &[]).await;
        assert_eq!(status, StatusCode::OK, "{name}");
        assert_eq!(headers[header::CONTENT_TYPE], content_type);
        assert_eq!(headers[header::CACHE_CONTROL], "private, max-age=86400");
        let image = image::load_from_memory(&body).unwrap();
        assert_eq!((image.width(), image.height()), (32, 24), "{name}");
        // Without a size, a frame smaller than the default keeps its size.
        let (_, _, body) = fetch_with(&app, &preview(name, ""), &[]).await;
        let image = image::load_from_memory(&body).unwrap();
        assert_eq!((image.width(), image.height()), (64, 48), "{name}");
    }

    // Instance colours are exact and match the studio's.
    let (_, _, body) = fetch_with(&app, &preview("instance.png", ""), &[]).await;
    let instance = image::load_from_memory(&body).unwrap().into_rgb8();
    assert_eq!(instance.get_pixel(0, 0).0, [78, 119, 218]);
    assert_eq!(instance.get_pixel(63, 47).0, [11, 12, 14]);
    // Depth background stays dark; JPEG is close, not exact.
    let (_, _, body) = fetch_with(&app, &preview("depth.exr", ""), &[]).await;
    let depth = image::load_from_memory(&body).unwrap().into_rgb8();
    let [r, g, b] = depth.get_pixel(2, 2).0;
    assert!(r < 30 && g < 30 && b < 30, "{:?}", [r, g, b]);

    for (uri, status) in [
        (preview("scene.json", ""), StatusCode::BAD_REQUEST),
        (preview("rgb.png", "?size=8"), StatusCode::BAD_REQUEST),
        (
            format!("/api/v1/jobs/{id}/previews/scenes/8/frames/00001/rgb.png"),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (actual, _, _) = fetch_with(&app, &uri, &[]).await;
        assert_eq!(actual, status, "{uri}");
    }
}

//! HTTP job server for dataset generation.
//!
//! Jobs are recorded in `SQLite` or `PostgreSQL`, run one at a time on this
//! machine's renderer, and stored in an S3-compatible bucket or a local
//! directory. See `docs/book/server.md`.

pub mod api;
pub mod auth;
pub mod db;
pub mod examples;
pub mod index;
pub mod runner;
pub mod settings;
pub mod storage;
pub mod submission;

use std::sync::Arc;

/// Connects storage and the database, and builds the router and its runner.
///
/// The runner is not started; spawn [`runner::Runner::run`].
///
/// # Errors
///
/// Returns a message naming the store that failed.
pub async fn build(settings: settings::Settings) -> Result<(axum::Router, runner::Runner), String> {
    let storage = storage::Storage::from_url(&settings.storage_url)
        .map_err(|error| format!("storage {}: {error}", settings.storage_url))?
        .with_concurrency(settings.storage_concurrency);
    // Fails fast on bad credentials, an unreachable endpoint, or a missing bucket.
    storage
        .exists(".bop-probe")
        .await
        .map_err(|error| format!("storage {}: {error}", settings.storage_url))?;
    let jobs = db::JobStore::connect(&settings.database_url)
        .await
        .map_err(|error| format!("database: {error}"))?;
    // A new library gets the example meshes the studio's presets use; the
    // server still starts without them.
    if settings.seed_examples {
        match examples::seed(&storage, &settings.examples_dir).await {
            Ok(copied) if !copied.is_empty() => {
                tracing::info!(assets = ?copied, "added example assets to the library");
            }
            Ok(_) => {}
            Err(error) => tracing::warn!("cannot add example assets: {error}"),
        }
    }
    // Picks up assets changed in storage directly while the server was down.
    let assets = index::reconcile_assets(&jobs, &storage)
        .await
        .map_err(|error| format!("asset index: {error}"))?;
    tracing::info!(assets, "indexed the asset library");
    // Jobs whose server stopped renewing their lease, and with
    // BOP_REQUEUE_ON_START the ones this server's last process left running.
    let released = jobs
        .release_interrupted(
            settings
                .requeue_on_start
                .then_some(settings.runner_id.as_str()),
        )
        .await
        .map_err(|error| format!("database: {error}"))?;
    if !released.is_empty() {
        tracing::info!(jobs = released.len(), "released interrupted jobs");
    }
    let auth = auth::Authenticator::new(&settings)?;
    if let Some(oidc) = &settings.oidc {
        // The provider may still be starting; keys are fetched again on first use.
        match auth.prefetch().await {
            Ok(()) => tracing::info!(issuer = %oidc.issuer, "accepting OIDC access tokens"),
            Err(error) => tracing::warn!(issuer = %oidc.issuer, "{error}"),
        }
    }
    if !auth.required() {
        tracing::warn!("the API is open to anyone who can reach it");
    }
    let settings = Arc::new(settings);
    let runner = runner::Runner::new(jobs.clone(), storage.clone(), Arc::clone(&settings));
    let router = api::router(api::AppState {
        jobs,
        storage,
        runner: runner.clone(),
        auth,
        settings,
    });
    Ok((router, runner))
}

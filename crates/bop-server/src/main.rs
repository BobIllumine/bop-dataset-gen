//! `bop-server`: serves the job API and runs jobs on this machine.

use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_env("BOP_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
    let settings = match bop_server::settings::Settings::from_env() {
        Ok(settings) => settings,
        Err(message) => {
            tracing::error!("{message}");
            return ExitCode::from(2);
        }
    };
    let listen = settings.listen;
    let (router, runner) = match bop_server::build(settings).await {
        Ok(built) => built,
        Err(message) => {
            tracing::error!("{message}");
            return ExitCode::from(3);
        }
    };
    let listener = match tokio::net::TcpListener::bind(listen).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!("cannot listen on {listen}: {error}");
            return ExitCode::from(3);
        }
    };
    let running = tokio::spawn(runner.clone().run());
    tracing::info!(%listen, "serving");
    let served = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_signal())
        .await;
    // In-flight scenes finish and commit; the job is queued for the next start.
    runner.shutdown();
    if let Err(error) = running.await {
        tracing::error!("runner stopped abnormally: {error}");
    }
    match served {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("server failed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
    tracing::info!("shutting down; in-flight scenes will finish first");
}

//! Ctrl-C and SIGTERM handling for the long-running commands.

use std::sync::{Mutex, OnceLock, PoisonError};

use bop_pipeline::CancellationToken;

/// Exit status of a run stopped by an interrupt (128 + SIGINT).
pub(crate) const INTERRUPTED: u8 = 130;

/// The token the next interrupt cancels.
static CURRENT: Mutex<Option<CancellationToken>> = Mutex::new(None);
static HANDLER: OnceLock<()> = OnceLock::new();

/// Returns a fresh token that the next Ctrl-C or SIGTERM cancels.
///
/// The first interrupt lets in-flight scenes finish and commit, so the run
/// returns a cancelled report and can be resumed. A second interrupt exits at
/// once; that is also safe because scenes are published by atomic rename.
pub(crate) fn cancel_on_interrupt() -> CancellationToken {
    HANDLER.get_or_init(|| {
        if let Err(error) = ctrlc::set_handler(on_interrupt) {
            tracing::warn!("cannot install the interrupt handler: {error}");
        }
    });
    let token = CancellationToken::new();
    *CURRENT.lock().unwrap_or_else(PoisonError::into_inner) = Some(token.clone());
    token
}

fn on_interrupt() {
    let current = CURRENT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    match current {
        Some(token) if !token.is_cancelled() => {
            token.cancel();
            tracing::warn!(
                "interrupt received; finishing in-flight scenes (interrupt again to stop now)"
            );
        }
        _ => {
            tracing::warn!("second interrupt; stopping without waiting for in-flight scenes");
            std::process::exit(i32::from(INTERRUPTED));
        }
    }
}

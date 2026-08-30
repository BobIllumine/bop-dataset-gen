use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// Cheap cloneable cooperative cancellation signal shared across pipeline owners.
///
/// Cancellation is monotonic. A scene already being simulated, rendered, or
/// atomically committed is allowed to finish; callers must check the token
/// before allocating the next unit of work.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Creates a token in the active state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cooperative cancellation for every clone.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Reports whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

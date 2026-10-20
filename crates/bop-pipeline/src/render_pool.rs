use std::sync::{
    Mutex, PoisonError,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use bop_core::{BackendInfo, SceneSnapshot};
use bop_render_api::Renderer;
use crossbeam_channel::Receiver;

use crate::{
    CancellationToken, PipelineRenderError, RenderRequestPlanner, RenderRequestPlanningError,
    SerialDatasetRenderer,
};

/// Renders snapshots from `receiver` on one thread per renderer until the
/// channel closes, cancellation is requested, or a renderer fails.
///
/// Each thread owns one renderer and commits whole scenes atomically, so
/// commit order does not matter. The first failure stops the other threads
/// before their next scene. Every receiver handle is dropped before this
/// returns, so a producer blocked on `send` is released. Returns the number of
/// scenes committed.
pub(crate) fn render_pool<R, E>(
    datasets: &mut [SerialDatasetRenderer<R>],
    requests: &RenderRequestPlanner,
    receiver: Receiver<SceneSnapshot>,
    cancellation: &CancellationToken,
) -> Result<u64, E>
where
    R: Renderer,
    E: From<RenderRequestPlanningError> + From<PipelineRenderError> + Send,
{
    let failed = AtomicBool::new(false);
    let first_error = Mutex::new(None);
    let committed = AtomicU64::new(0);
    std::thread::scope(|scope| {
        for dataset in datasets.iter_mut() {
            let receiver = receiver.clone();
            let (failed, first_error, committed) = (&failed, &first_error, &committed);
            scope.spawn(move || {
                for scene in &receiver {
                    if cancellation.is_cancelled() || failed.load(Ordering::Acquire) {
                        break;
                    }
                    let outcome = requests
                        .plan(scene)
                        .map_err(E::from)
                        .and_then(|request| dataset.render_and_commit(&request).map_err(E::from));
                    match outcome {
                        Ok(_) => {
                            committed.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(error) => {
                            first_error
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .get_or_insert(error);
                            failed.store(true, Ordering::Release);
                            break;
                        }
                    }
                }
            });
        }
        drop(receiver);
    });
    match first_error
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
    {
        Some(error) => Err(error),
        None => Ok(committed.into_inner()),
    }
}

/// Wraps each renderer for transactional output and checks they share one identity.
///
/// The dataset manifest records a single backend, so every renderer in a pool
/// must report the same provenance.
pub(crate) fn dataset_renderers<R: Renderer>(
    renderers: impl IntoIterator<Item = R>,
    dataset_root: &std::path::Path,
) -> Result<(Vec<SerialDatasetRenderer<R>>, BackendInfo), RendererPoolError> {
    let datasets = renderers
        .into_iter()
        .map(|renderer| SerialDatasetRenderer::new(renderer, dataset_root))
        .collect::<Vec<_>>();
    let info = datasets
        .first()
        .ok_or(RendererPoolError::Empty)?
        .renderer()
        .info();
    if datasets
        .iter()
        .any(|dataset| dataset.renderer().info() != info)
    {
        return Err(RendererPoolError::MismatchedBackends);
    }
    Ok((datasets, info))
}

/// The renderers handed to a parallel pipeline cannot form one pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RendererPoolError {
    /// No renderer was supplied.
    #[error("at least one renderer is required")]
    Empty,
    /// Renderers reported different backend provenance.
    #[error("every renderer in a pool must report the same backend provenance")]
    MismatchedBackends,
}

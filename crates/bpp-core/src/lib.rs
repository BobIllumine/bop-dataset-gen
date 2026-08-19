//! Backend-neutral domain contracts for BPP dataset generation.

mod identifiers;
mod transforms;

pub use identifiers::{AssetId, AttemptId, CameraId, InstanceId, SceneId};
pub use transforms::{Meters, RigidTransform};

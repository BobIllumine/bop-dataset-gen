//! Backend-neutral domain contracts for BPP dataset generation.

mod identifiers;
mod scenes;
mod transforms;

pub use identifiers::{
    AssetId, AttemptId, CameraId, InstanceId, InvalidAssetIdError, InvalidInstanceIdError,
    InvalidSchemaVersionError, SceneId, SchemaVersion,
};
pub use scenes::{
    BinSnapshot, InstanceSnapshot, InvalidBinSnapshotError, InvalidPhysicsReportError,
    InvalidPositiveMetersError, InvalidResolvedPhysicalPropertiesError, InvalidSceneSnapshotError,
    PhysicsRejectionReason, PhysicsReport, PositiveMeters, RejectionCount,
    ResolvedPhysicalProperties, SceneSnapshot,
};
pub use transforms::{
    CameraExtrinsics, InvalidRigidTransformError, Meters, RigidTransform, z_up_camera_to_opencv,
};

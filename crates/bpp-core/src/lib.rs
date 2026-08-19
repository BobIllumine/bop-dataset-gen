//! Backend-neutral domain contracts for BPP dataset generation.

mod appearances;
mod cameras;
mod identifiers;
mod render_reports;
mod render_requests;
mod scenes;
mod transforms;

pub use appearances::{
    AmbientLight, AppearanceSnapshot, DirectionalLight, InvalidDirectionalLightError,
    InvalidLinearRgbError, InvalidNonNegativeFiniteError, InvalidSrgbColorError, LinearRgb,
    NonNegativeFinite, PointLight, SrgbColor,
};
pub use cameras::{
    CameraIntrinsics, CameraSnapshot, ImageSize, InvalidCameraIntrinsicsError,
    InvalidCameraSnapshotError, InvalidImageSizeError,
};
pub use identifiers::{
    AssetId, AttemptId, CameraId, InstanceId, InvalidAssetIdError, InvalidInstanceIdError,
    InvalidSchemaVersionError, SceneId, SchemaVersion,
};
pub use render_reports::{
    BackendInfo, BackendKind, InvalidBackendInfoError, RenderReport, RenderStageTimings,
    RendererCapabilities,
};
pub use render_requests::{
    InvalidRenderChannelsError, InvalidRenderSceneRequestError, RenderChannels, RenderSceneRequest,
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

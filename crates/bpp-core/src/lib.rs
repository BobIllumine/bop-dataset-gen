//! Backend-neutral domain contracts for BPP dataset generation.

mod appearances;
mod cameras;
mod deterministic_planning;
mod distributions;
mod identifiers;
mod render_error;
mod render_reports;
mod render_requests;
mod run_config;
mod run_config_wire;
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
pub use deterministic_planning::{
    InvalidScenePlanError, SceneJob, ScenePlanIter, SeedDeriver, SeedStage,
};
pub use distributions::{
    Distribution, DistributionView, InvalidDistributionError, InvalidWeightedValueError,
    UniformBound, UniformSemantics, WeightedValue,
};
pub use identifiers::{
    AssetId, AttemptId, CameraId, InstanceId, InvalidAssetIdError, InvalidInstanceIdError,
    InvalidSchemaVersionError, SceneId, SchemaVersion,
};
pub use render_error::RenderError;
pub use render_reports::{
    BackendInfo, BackendKind, InvalidBackendInfoError, RenderReport, RenderStageTimings,
    RendererCapabilities,
};
pub use render_requests::{
    InvalidRenderChannelsError, InvalidRenderSceneRequestError, RenderChannels, RenderSceneRequest,
};
pub use run_config::{
    AppearanceConfig, AppearanceField, AssetConfig, Axis, BinConfig, BlenderProcRendererConfig,
    CameraConfig, CameraPose, CameraProjection, CameraShellField, ConfigCountField,
    ConfigDistributionError, ConfigPathField, ConfigScalarField, ConfigTextField,
    ConfigTransformField, DistributionField, InvalidBlenderProcRendererConfigError,
    InvalidWgpuRendererConfigError, PbrMaterialConfig, PhysicalPropertyDistributions,
    PhysicsConfig, PopulationConfig, RendererConfigs, RunConfig, RunConfigError,
    RunConfigValidationError, RunPlan, RunSettings, ShellCameraPose, SpawnVolume,
    SurfacePhysicalProperties, VerticalFovDegrees, VhacdParameters, WgpuAdapterPreference,
    WgpuDevicePreference, WgpuRendererConfig,
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

/// Generates the deterministic JSON Schema for the unchecked v1 TOML input shape.
///
/// The schema describes the private wire representation accepted by
/// [`RunConfig::from_toml`]. Cross-field semantic constraints remain enforced
/// by run-configuration validation after deserialization.
#[cfg(feature = "schema")]
#[must_use]
pub fn run_config_json_schema() -> String {
    run_config_wire::json_schema()
}

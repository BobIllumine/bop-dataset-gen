//! Typed configuration errors and the fields they name.

#[allow(
    clippy::wildcard_imports,
    reason = "submodules share the parent's imports and helpers"
)]
use super::*;

/// Top-level TOML loading failure.
#[derive(Debug, Error)]
pub enum RunConfigError {
    /// TOML syntax or strict object-shape validation failed.
    #[error("invalid TOML configuration: {0}")]
    Toml(#[from] toml::de::Error),
    /// Parsed values violated a typed semantic invariant.
    #[error("invalid run configuration: {0}")]
    Validation(#[from] RunConfigValidationError),
}

/// Work-count field associated with a zero-value validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigCountField {
    /// Requested valid scene count.
    ValidSceneCount,
    /// Attempts per requested scene.
    SceneAttemptLimit,
    /// Placement attempts per object.
    PlacementAttemptLimitPerObject,
    /// Simulation worker count.
    SimulationWorkers,
    /// Renderer worker count.
    RendererWorkers,
    /// Bounded queue capacity.
    QueueCapacity,
    /// V-HACD voxel resolution.
    VhacdResolution,
    /// V-HACD maximum hull count.
    VhacdMaxConvexHulls,
    /// V-HACD plane downsampling factor.
    VhacdPlaneDownsampling,
    /// V-HACD convex-hull downsampling factor.
    VhacdConvexHullDownsampling,
    /// Physics substeps.
    PhysicsSubsteps,
    /// Physics solver iterations.
    PhysicsSolverIterations,
    /// Physics minimum steps.
    PhysicsMinSteps,
    /// Physics maximum steps.
    PhysicsMaxSteps,
    /// Physics stability-check interval.
    PhysicsStabilityCheckInterval,
    /// Required stable checks.
    PhysicsRequiredStableChecks,
    /// wgpu shadow-map size.
    ShadowMapSize,
    /// `BlenderProc` stderr tail capacity.
    StderrTailCapacity,
    /// `BlenderProc` samples per pixel.
    BlenderProcSamples,
}

/// Path field associated with an empty-path validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigPathField {
    /// Dataset output path.
    Output,
    /// Asset cache path.
    Cache,
    /// Asset source path.
    AssetSource,
    /// `BlenderProc` executable path.
    BlenderProcExecutable,
    /// `BlenderProc` worker path.
    BlenderProcWorker,
    /// Appearance environment map.
    Environment,
}

/// Text field associated with a blank-text validation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigTextField {
    /// Bin material label.
    BinMaterialLabel,
    /// `BlenderProc` device-selection value.
    BlenderProcDeviceSelection,
}

/// Scalar field associated with a numeric domain failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigScalarField {
    /// Bin inner X dimension.
    BinInnerX,
    /// Bin inner Y dimension.
    BinInnerY,
    /// Bin inner Z dimension.
    BinInnerZ,
    /// Bin floor thickness.
    BinFloorThickness,
    /// Bin wall thickness.
    BinWallThickness,
    /// Bin friction.
    BinFriction,
    /// Bin restitution.
    BinRestitution,
    /// Bin render-material metallic factor.
    BinMetallic,
    /// Bin render-material roughness factor.
    BinRoughness,
    /// Asset source-unit scale.
    AssetSourceUnitScale,
    /// V-HACD concavity.
    VhacdConcavity,
    /// Physics timestep.
    PhysicsTimestep,
    /// Physics linear sleep threshold.
    PhysicsLinearSleepThreshold,
    /// Physics angular sleep threshold.
    PhysicsAngularSleepThreshold,
    /// Physics escape guard margin.
    PhysicsEscapeMargin,
    /// Physics maximum accepted final penetration.
    PhysicsMaximumPenetration,
    /// Camera near plane.
    CameraNear,
    /// Camera far plane.
    CameraFar,
    /// `BlenderProc` handshake timeout.
    BlenderProcHandshakeTimeout,
    /// `BlenderProc` request timeout.
    BlenderProcRequestTimeout,
    /// `BlenderProc` adaptive-sampling noise threshold.
    BlenderProcNoiseThreshold,
    /// wgpu ambient-occlusion radius.
    WgpuAmbientOcclusionRadius,
    /// Environment-map intensity.
    EnvironmentIntensity,
    /// Environment-map rotation.
    EnvironmentRotation,
}

/// Transform field associated with a malformed rigid transform.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigTransformField {
    /// Bin world pose.
    Bin,
    /// Asset model transform.
    AssetModel,
    /// Fixed camera world pose.
    CameraFixedPose,
}

/// Distribution field associated with a checked distribution failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DistributionField {
    /// Rigid-body mass in kilograms.
    MassKg,
    /// Friction coefficient.
    Friction,
    /// Restitution coefficient.
    Restitution,
    /// PBR metallic factor.
    Metallic,
    /// PBR roughness factor.
    Roughness,
    /// Population asset mixture.
    AssetMixture,
    /// Population object count.
    ObjectCount,
}

/// Appearance field associated with invalid checked color or light data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppearanceField {
    /// Scene background color.
    Background,
    /// Ambient color or intensity.
    Ambient,
    /// Directional light entry.
    DirectionalLight,
    /// Point light entry.
    PointLight,
    /// PBR base color.
    BaseColor,
    /// Bin render-material base color.
    BinBaseColor,
}

/// Shell field associated with invalid bounds or target data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CameraShellField {
    /// Radius bounds.
    Radius,
    /// Azimuth bounds.
    Azimuth,
    /// Elevation bounds.
    Elevation,
    /// Target point.
    Target,
}

/// Typed reason a raw distribution could not become an invariant-preserving value.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum ConfigDistributionError {
    /// The distribution's own variant invariant failed.
    #[error(transparent)]
    Distribution(#[from] InvalidDistributionError),
    /// A choice weight was non-finite or negative.
    #[error("choice entry {index} has an invalid weight: {source}")]
    Weight {
        /// Zero-based choice entry index.
        index: usize,
        /// Checked weight failure.
        source: InvalidWeightedValueError,
    },
    /// A finite distribution value violated the field's physical domain.
    #[error("distribution value {value} violates the field domain")]
    DomainValue {
        /// Rejected value.
        value: f32,
    },
}

/// Typed semantic validation failure for a parsed run configuration.
#[derive(Clone, Debug, Error, PartialEq)]
pub enum RunConfigValidationError {
    /// A work-controlling count or capacity was zero.
    #[error("{field:?} must be nonzero")]
    ZeroCount {
        /// Rejected field.
        field: ConfigCountField,
    },
    /// A configured path was empty or whitespace-only.
    #[error("{field:?} path must be nonempty")]
    InvalidPath {
        /// Rejected path field.
        field: ConfigPathField,
        /// Asset context for asset source paths.
        asset_id: Option<AssetId>,
    },
    /// A configured text field was empty or whitespace-only.
    #[error("{field:?} text must be nonblank")]
    InvalidText {
        /// Rejected text field.
        field: ConfigTextField,
    },
    /// A scalar required to be finite and positive was invalid.
    #[error("{field:?} must be finite and positive, got {value}")]
    InvalidPositiveScalar {
        /// Rejected scalar field.
        field: ConfigScalarField,
        /// Rejected value.
        value: f64,
    },
    /// A scalar required to be finite and nonnegative was invalid.
    #[error("{field:?} must be finite and nonnegative, got {value}")]
    InvalidNonNegativeScalar {
        /// Rejected scalar field.
        field: ConfigScalarField,
        /// Rejected value.
        value: f64,
    },
    /// A scalar required to be finite was not.
    #[error("{field:?} must be finite, got {value}")]
    InvalidFiniteScalar {
        /// Rejected scalar field.
        field: ConfigScalarField,
        /// Rejected value.
        value: f64,
    },
    /// A scalar required to be finite and in `[0, 1]` was invalid.
    #[error("{field:?} must be finite and in [0, 1], got {value}")]
    InvalidUnitInterval {
        /// Rejected scalar field.
        field: ConfigScalarField,
        /// Rejected value.
        value: f64,
    },
    /// A rigid transform was malformed.
    #[error("{field:?} rigid transform is invalid")]
    InvalidTransform {
        /// Rejected transform field.
        field: ConfigTransformField,
        /// Optional asset context.
        asset_id: Option<AssetId>,
        /// Optional camera context.
        camera_id: Option<CameraId>,
    },
    /// An asset ID was blank.
    #[error("asset ID must be nonblank, got {value:?}")]
    InvalidAssetId {
        /// Rejected raw ID.
        value: String,
    },
    /// An asset catalog was empty.
    #[error("assets must contain at least one entry")]
    EmptyAssets,
    /// Two asset entries used the same ID.
    #[error("duplicate asset ID {asset_id:?}")]
    DuplicateAssetId {
        /// Repeated asset ID.
        asset_id: AssetId,
    },
    /// Every catalog mixture weight was zero.
    #[error("at least one asset mixture weight must be positive")]
    AllZeroAssetWeights,
    /// One asset mixture weight was non-finite or negative.
    #[error("asset {asset_id:?} has invalid mixture weight {value}")]
    InvalidAssetMixtureWeight {
        /// Asset owning the rejected weight.
        asset_id: AssetId,
        /// Rejected weight.
        value: f64,
    },
    /// A typed distribution could not be validated.
    #[error("invalid {field:?} distribution: {source}")]
    InvalidDistribution {
        /// Distribution's semantic role.
        field: DistributionField,
        /// Optional owning asset.
        asset_id: Option<AssetId>,
        /// Typed distribution failure.
        source: ConfigDistributionError,
    },
    /// A population asset-mixture ID was blank.
    #[error("population asset reference must be nonblank, got {value:?}")]
    InvalidAssetReference {
        /// Rejected raw reference.
        value: String,
    },
    /// A population asset mixture referenced no configured asset.
    #[error("unknown asset reference {asset_id:?}")]
    UnknownAssetReference {
        /// Unknown asset ID.
        asset_id: AssetId,
    },
    /// The population distribution admitted zero objects or inverted bounds.
    #[error(
        "population object count must contain only positive ordered values, got {minimum}..={maximum}"
    )]
    InvalidPopulationCount {
        /// Lowest observed or configured count.
        minimum: u32,
        /// Highest observed or configured count.
        maximum: u32,
    },
    /// A spawn interval was non-finite, empty, or inverted.
    #[error("spawn interval on {axis:?} must be finite with min < max")]
    InvalidSpawnAxis {
        /// Rejected axis.
        axis: Axis,
    },
    /// The configuration did not describe its bins as either `bin` with
    /// `population`, or a nonempty `bins` list.
    #[error("configure either [bin] with [population], or one or more [[bins]]")]
    BinLayout,
    /// A bin identifier was blank.
    #[error("bin id must be nonblank, got {value:?}")]
    InvalidBinId {
        /// Rejected identifier.
        value: String,
    },
    /// Two bins used the same identifier.
    #[error("duplicate bin id {bin_id:?}")]
    DuplicateBinId {
        /// Repeated identifier.
        bin_id: String,
    },
    /// The outer boxes of two bins intersect.
    #[error("bins {first:?} and {second:?} overlap")]
    OverlappingBins {
        /// Earlier bin.
        first: String,
        /// Later bin.
        second: String,
    },
    /// A spawn interval extended beyond the bin interior.
    #[error("spawn interval on {axis:?} extends outside the bin interior")]
    SpawnOutsideBin {
        /// Rejected axis.
        axis: Axis,
    },
    /// Physics minimum steps exceeded maximum steps.
    #[error("physics min_steps {min_steps} exceeds max_steps {max_steps}")]
    InvalidPhysicsOrdering {
        /// Configured minimum.
        min_steps: u32,
        /// Configured maximum.
        max_steps: u32,
    },
    /// Required stability observations cannot fit in the step budget.
    #[error(
        "{required_checks} stability checks every {interval} steps exceed max_steps {max_steps}"
    )]
    StabilityWindowExceedsMaximum {
        /// Check interval in steps.
        interval: u32,
        /// Required observation count.
        required_checks: u32,
        /// Maximum simulation steps.
        max_steps: u32,
    },
    /// No camera was configured.
    #[error("cameras must contain at least one entry")]
    EmptyCameras,
    /// Two cameras used the same stable ID.
    #[error("duplicate camera ID {camera_id:?}")]
    DuplicateCameraId {
        /// Repeated camera ID.
        camera_id: CameraId,
    },
    /// A camera image size contained a zero dimension.
    #[error("camera {camera_id:?} image size must be nonzero")]
    InvalidCameraImageSize {
        /// Owning camera.
        camera_id: CameraId,
    },
    /// A camera clipping plane was invalid or the near plane was not before far.
    #[error("camera {camera_id:?} clipping planes must be positive with near < far")]
    InvalidCameraClipping {
        /// Owning camera.
        camera_id: CameraId,
    },
    /// A camera configured zero or multiple projection sources.
    #[error("camera {camera_id:?} must configure exactly one projection source, got {configured}")]
    InvalidCameraProjectionSources {
        /// Owning camera.
        camera_id: CameraId,
        /// Number of configured sources.
        configured: u8,
    },
    /// A camera configured zero or multiple pose sources.
    #[error("camera {camera_id:?} must configure exactly one pose source, got {configured}")]
    InvalidCameraPoseSources {
        /// Owning camera.
        camera_id: CameraId,
        /// Number of configured sources.
        configured: u8,
    },
    /// An explicit `OpenCV` matrix was invalid.
    #[error("camera {camera_id:?} OpenCV intrinsics are invalid")]
    InvalidCameraIntrinsics {
        /// Owning camera.
        camera_id: CameraId,
    },
    /// A vertical field of view was not finite and strictly between 0 and 180.
    #[error("camera {camera_id:?} vertical FOV must be finite and in (0, 180), got {value}")]
    InvalidVerticalFov {
        /// Owning camera.
        camera_id: CameraId,
        /// Rejected field of view.
        value: f32,
    },
    /// A shell pose field was invalid.
    #[error("camera {camera_id:?} has invalid shell {field:?}")]
    InvalidCameraShell {
        /// Owning camera.
        camera_id: CameraId,
        /// Rejected shell field.
        field: CameraShellField,
    },
    /// Appearance data failed a checked color/light invariant.
    #[error("invalid appearance {field:?} at index {index:?}")]
    InvalidAppearance {
        /// Rejected appearance field.
        field: AppearanceField,
        /// Optional light-list index.
        index: Option<usize>,
    },
    /// Every output channel was disabled.
    #[error("at least one output channel must be requested")]
    NoOutputChannels,
    /// Visible-instance labels cannot represent the maximum population.
    #[error("visible-instance output supports at most 65535 objects, got {maximum}")]
    ExcessiveInstanceCount {
        /// Maximum possible object count.
        maximum: u32,
    },
}

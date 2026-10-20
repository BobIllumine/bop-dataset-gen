use serde::Deserialize;

use super::{BlenderProcDenoiser, BlenderProcSampling, WgpuRendererConfig};

pub(super) const DEFAULT_SCENE_ATTEMPT_LIMIT: u32 = 20;
pub(super) const DEFAULT_PLACEMENT_ATTEMPT_LIMIT: u32 = 100;
pub(super) const DEFAULT_FIXED_TIMESTEP_S: f32 = 1.0 / 240.0;
pub(super) const DEFAULT_MIN_STEPS: u32 = 120;
pub(super) const DEFAULT_MAX_STEPS: u32 = 4_800;
pub(super) const DEFAULT_STABILITY_CHECK_INTERVAL: u32 = 24;
pub(super) const DEFAULT_LINEAR_SLEEP_THRESHOLD_MPS: f32 = 0.01;
pub(super) const DEFAULT_ANGULAR_SLEEP_THRESHOLD_RAD_S: f32 = 0.05;
pub(super) const DEFAULT_REQUIRED_STABLE_CHECKS: u32 = 10;
pub(super) const DEFAULT_ESCAPE_MARGIN_M: f32 = 0.002;
pub(super) const DEFAULT_MAXIMUM_PENETRATION_M: f32 = 0.002;

/// The complete unchecked v1 TOML input shape.
///
/// Local scalar and structural constraints are represented in this schema.
/// Cross-field ordering, references, uniqueness, and aggregate constraints are
/// enforced by `RunConfig` validation after deserialization.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "schema",
    schemars(extend("oneOf" = [
        { "required": ["bin", "population"], "not": { "required": ["bins"] } },
        { "required": ["bins"], "not": { "anyOf": [{ "required": ["bin"] }, { "required": ["population"] }] } }
    ]))
)]
#[cfg_attr(feature = "schema", schemars(rename = "RunConfigV1"))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RunConfigWire {
    pub(super) run: RunWire,
    /// The single bin. Use either `bin` with `population`, or `bins`.
    #[serde(default)]
    #[cfg_attr(feature = "schema", schemars(with = "BinWire"))]
    pub(super) bin: Option<BinWire>,
    #[cfg_attr(feature = "schema", schemars(length(min = 1)))]
    pub(super) assets: Vec<AssetWire>,
    /// Objects dropped into the single `bin`.
    #[serde(default)]
    #[cfg_attr(feature = "schema", schemars(with = "PopulationWire"))]
    pub(super) population: Option<PopulationWire>,
    /// Several bins, each with its own population. Replaces `bin` and `population`.
    #[serde(default)]
    #[cfg_attr(
        feature = "schema",
        schemars(with = "Vec<BinEntryWire>", length(min = 1))
    )]
    pub(super) bins: Option<Vec<BinEntryWire>>,
    pub(super) physics: PhysicsWire,
    #[cfg_attr(feature = "schema", schemars(length(min = 1)))]
    pub(super) cameras: Vec<CameraWire>,
    pub(super) appearance: AppearanceWire,
    pub(super) renderers: RenderersWire,
    pub(super) output: OutputWire,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RunWire {
    pub(super) master_seed: u64,
    #[cfg_attr(feature = "schema", schemars(range(min = 1)))]
    pub(super) valid_scene_count: u32,
    #[cfg_attr(
        feature = "schema",
        schemars(
            range(min = 1),
            extend("default" = DEFAULT_SCENE_ATTEMPT_LIMIT)
        )
    )]
    pub(super) scene_attempt_limit: Option<u32>,
    #[cfg_attr(
        feature = "schema",
        schemars(
            range(min = 1),
            extend("default" = DEFAULT_PLACEMENT_ATTEMPT_LIMIT)
        )
    )]
    pub(super) placement_attempt_limit_per_object: Option<u32>,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveU32Schema"))]
    pub(super) simulation_workers: u32,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveU32Schema"))]
    pub(super) renderer_workers: u32,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveU32Schema"))]
    pub(super) queue_capacity: u32,
    #[cfg_attr(feature = "schema", schemars(with = "NonBlankStringSchema"))]
    pub(super) output_path: String,
    #[cfg_attr(feature = "schema", schemars(with = "NonBlankStringSchema"))]
    pub(super) cache_path: String,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BinWire {
    #[cfg_attr(feature = "schema", schemars(with = "[PositiveF32Schema; 3]"))]
    pub(super) inner_dimensions_m: [f32; 3],
    #[cfg_attr(feature = "schema", schemars(with = "PositiveF32Schema"))]
    pub(super) floor_thickness_m: f32,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveF32Schema"))]
    pub(super) wall_thickness_m: f32,
    pub(super) world_from_bin: TransformWire,
    #[cfg_attr(feature = "schema", schemars(with = "NonBlankStringSchema"))]
    pub(super) material_label: String,
    pub(super) physical_properties: SurfacePhysicalPropertiesWire,
    #[serde(default)]
    pub(super) render_material: Option<BinRenderMaterialWire>,
}

/// One of several bins: the `[bin]` fields plus an identifier and a population.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BinEntryWire {
    /// Unique, nonblank; recorded on every object dropped into this bin.
    #[cfg_attr(feature = "schema", schemars(with = "NonBlankStringSchema"))]
    pub(super) id: String,
    #[cfg_attr(feature = "schema", schemars(with = "[PositiveF32Schema; 3]"))]
    pub(super) inner_dimensions_m: [f32; 3],
    #[cfg_attr(feature = "schema", schemars(with = "PositiveF32Schema"))]
    pub(super) floor_thickness_m: f32,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveF32Schema"))]
    pub(super) wall_thickness_m: f32,
    pub(super) world_from_bin: TransformWire,
    #[cfg_attr(feature = "schema", schemars(with = "NonBlankStringSchema"))]
    pub(super) material_label: String,
    pub(super) physical_properties: SurfacePhysicalPropertiesWire,
    #[serde(default)]
    pub(super) render_material: Option<BinRenderMaterialWire>,
    pub(super) population: PopulationWire,
}

impl BinEntryWire {
    pub(super) fn split(self) -> (String, BinWire, PopulationWire) {
        (
            self.id,
            BinWire {
                inner_dimensions_m: self.inner_dimensions_m,
                floor_thickness_m: self.floor_thickness_m,
                wall_thickness_m: self.wall_thickness_m,
                world_from_bin: self.world_from_bin,
                material_label: self.material_label,
                physical_properties: self.physical_properties,
                render_material: self.render_material,
            },
            self.population,
        )
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BinRenderMaterialWire {
    #[cfg_attr(feature = "schema", schemars(with = "[UnitF32Schema; 4]"))]
    pub(super) base_color_srgb: [f32; 4],
    #[cfg_attr(feature = "schema", schemars(with = "UnitF32Schema"))]
    pub(super) metallic: f32,
    #[cfg_attr(feature = "schema", schemars(with = "UnitF32Schema"))]
    pub(super) roughness: f32,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SurfacePhysicalPropertiesWire {
    #[cfg_attr(feature = "schema", schemars(with = "NonNegativeF32Schema"))]
    pub(super) friction: f32,
    #[cfg_attr(feature = "schema", schemars(with = "UnitF32Schema"))]
    pub(super) restitution: f32,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TransformWire {
    pub(super) translation: [f32; 3],
    pub(super) rotation_xyzw: [f32; 4],
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AssetWire {
    #[cfg_attr(feature = "schema", schemars(with = "NonBlankStringSchema"))]
    pub(super) id: String,
    #[cfg_attr(feature = "schema", schemars(with = "NonBlankStringSchema"))]
    pub(super) source_path: String,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveF32Schema"))]
    pub(super) source_unit_scale_m: f32,
    pub(super) model_transform: TransformWire,
    #[cfg_attr(feature = "schema", schemars(with = "NonNegativeF64Schema"))]
    pub(super) mixture_weight: f64,
    pub(super) physical_properties: PhysicalPropertyDistributionsWire,
    pub(super) visual_override: Option<PbrMaterialWire>,
    pub(super) vhacd: VhacdWire,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PhysicalPropertyDistributionsWire {
    #[cfg_attr(
        feature = "schema",
        schemars(with = "DistributionSchema<PositiveF32Schema>")
    )]
    pub(super) mass_kg: DistributionWire<f32>,
    #[cfg_attr(
        feature = "schema",
        schemars(with = "DistributionSchema<NonNegativeF32Schema>")
    )]
    pub(super) friction: DistributionWire<f32>,
    #[cfg_attr(
        feature = "schema",
        schemars(with = "DistributionSchema<UnitF32Schema>")
    )]
    pub(super) restitution: DistributionWire<f32>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PbrMaterialWire {
    #[cfg_attr(feature = "schema", schemars(with = "[UnitF32Schema; 4]"))]
    pub(super) base_color_srgb: [f32; 4],
    #[cfg_attr(
        feature = "schema",
        schemars(with = "DistributionSchema<UnitF32Schema>")
    )]
    pub(super) metallic: DistributionWire<f32>,
    #[cfg_attr(
        feature = "schema",
        schemars(with = "DistributionSchema<UnitF32Schema>")
    )]
    pub(super) roughness: DistributionWire<f32>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VhacdWire {
    #[cfg_attr(feature = "schema", schemars(with = "PositiveU32Schema"))]
    pub(super) resolution: u32,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveU32Schema"))]
    pub(super) max_convex_hulls: u32,
    #[cfg_attr(feature = "schema", schemars(with = "NonNegativeF32Schema"))]
    pub(super) concavity: f32,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveU32Schema"))]
    pub(super) plane_downsampling: u32,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveU32Schema"))]
    pub(super) convex_hull_downsampling: u32,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PopulationWire {
    #[cfg_attr(
        feature = "schema",
        schemars(with = "DistributionSchema<PositiveU32Schema>")
    )]
    pub(super) object_count: DistributionWire<u32>,
    #[cfg_attr(feature = "schema", schemars(with = "AssetDistributionSchema"))]
    pub(super) asset_mixture: DistributionWire<String>,
    /// Bounds must be strictly ordered and contained by the configured bin.
    pub(super) spawn_volume: SpawnVolumeWire,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SpawnVolumeWire {
    pub(super) min_m: [f32; 3],
    pub(super) max_m: [f32; 3],
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PhysicsWire {
    #[cfg_attr(
        feature = "schema",
        schemars(
            extend(
                "exclusiveMinimum" = 0.0,
                "default" = DEFAULT_FIXED_TIMESTEP_S
            )
        )
    )]
    pub(super) fixed_timestep_s: Option<f32>,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveU32Schema"))]
    pub(super) substeps: u32,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveU32Schema"))]
    pub(super) solver_iterations: u32,
    /// Must not exceed `max_steps`.
    #[cfg_attr(
        feature = "schema",
        schemars(
            range(min = 1),
            extend("default" = DEFAULT_MIN_STEPS)
        )
    )]
    pub(super) min_steps: Option<u32>,
    #[cfg_attr(
        feature = "schema",
        schemars(
            range(min = 1),
            extend("default" = DEFAULT_MAX_STEPS)
        )
    )]
    pub(super) max_steps: Option<u32>,
    #[cfg_attr(
        feature = "schema",
        schemars(
            range(min = 1),
            extend("default" = DEFAULT_STABILITY_CHECK_INTERVAL)
        )
    )]
    pub(super) stability_check_interval: Option<u32>,
    #[cfg_attr(
        feature = "schema",
        schemars(
            range(min = 0.0),
            extend("default" = DEFAULT_LINEAR_SLEEP_THRESHOLD_MPS)
        )
    )]
    pub(super) linear_sleep_threshold_mps: Option<f32>,
    #[cfg_attr(
        feature = "schema",
        schemars(
            range(min = 0.0),
            extend("default" = DEFAULT_ANGULAR_SLEEP_THRESHOLD_RAD_S)
        )
    )]
    pub(super) angular_sleep_threshold_rad_s: Option<f32>,
    /// The stability interval times this count must not exceed `max_steps`.
    #[cfg_attr(
        feature = "schema",
        schemars(
            range(min = 1),
            extend("default" = DEFAULT_REQUIRED_STABLE_CHECKS)
        )
    )]
    pub(super) required_stable_checks: Option<u32>,
    #[cfg_attr(
        feature = "schema",
        schemars(range(min = 0.0), extend("default" = DEFAULT_ESCAPE_MARGIN_M))
    )]
    pub(super) escape_margin_m: Option<f32>,
    #[cfg_attr(
        feature = "schema",
        schemars(
            range(min = 0.0),
            extend("default" = DEFAULT_MAXIMUM_PENETRATION_M)
        )
    )]
    pub(super) maximum_penetration_m: Option<f32>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CameraWire {
    pub(super) id: u16,
    pub(super) image_size: ImageSizeWire,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveF32Schema"))]
    pub(super) near_m: f32,
    /// Must be greater than `near_m`.
    #[cfg_attr(feature = "schema", schemars(with = "PositiveF32Schema"))]
    pub(super) far_m: f32,
    #[cfg_attr(feature = "schema", schemars(with = "CameraProjectionSchema"))]
    pub(super) projection: CameraProjectionWire,
    #[cfg_attr(feature = "schema", schemars(with = "CameraPoseSchema"))]
    pub(super) pose: CameraPoseWire,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImageSizeWire {
    #[cfg_attr(feature = "schema", schemars(with = "PositiveU32Schema"))]
    pub(super) width: u32,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveU32Schema"))]
    pub(super) height: u32,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CameraProjectionWire {
    pub(super) opencv_k: Option<[f32; 9]>,
    pub(super) vertical_fov_deg: Option<f32>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CameraPoseWire {
    pub(super) fixed_world_from_camera: Option<TransformWire>,
    pub(super) shell: Option<ShellCameraPoseWire>,
}

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
enum CameraProjectionSchema {
    OpenCvK {
        opencv_k: [f32; 9],
    },
    VerticalFov {
        #[schemars(with = "VerticalFovSchema")]
        vertical_fov_deg: f32,
    },
}

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
enum CameraPoseSchema {
    Fixed {
        fixed_world_from_camera: TransformWire,
    },
    Shell {
        shell: ShellCameraPoseWire,
    },
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ShellCameraPoseWire {
    #[cfg_attr(feature = "schema", schemars(with = "PositiveF32Schema"))]
    pub(super) min_radius_m: f32,
    /// Must be greater than or equal to `min_radius_m`.
    #[cfg_attr(feature = "schema", schemars(with = "PositiveF32Schema"))]
    pub(super) max_radius_m: f32,
    pub(super) min_azimuth_deg: f32,
    pub(super) max_azimuth_deg: f32,
    pub(super) min_elevation_deg: f32,
    pub(super) max_elevation_deg: f32,
    pub(super) target_bin_m: [f32; 3],
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AppearanceWire {
    #[cfg_attr(feature = "schema", schemars(with = "[UnitF32Schema; 4]"))]
    pub(super) background_srgb: [f32; 4],
    pub(super) ambient: AmbientWire,
    pub(super) directional_lights: Vec<DirectionalLightWire>,
    pub(super) point_lights: Vec<PointLightWire>,
    pub(super) material_defaults: PbrMaterialWire,
    /// An HDR environment map (`.hdr` or `.exr`, equirectangular) lighting the
    /// scene instead of `ambient`.
    #[serde(default)]
    pub(super) environment: Option<EnvironmentWire>,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EnvironmentWire {
    /// Image path, relative to the configuration file.
    #[cfg_attr(feature = "schema", schemars(with = "NonBlankStringSchema"))]
    pub(super) path: String,
    /// Radiance scale. Default 1.
    #[serde(default = "default_environment_intensity")]
    #[cfg_attr(feature = "schema", schemars(range(min = 0.0)))]
    pub(super) intensity: f32,
    /// Rotation about world +Z in degrees. Default 0.
    #[serde(default)]
    pub(super) rotation_deg: f32,
}

const fn default_environment_intensity() -> f32 {
    1.0
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AmbientWire {
    #[cfg_attr(feature = "schema", schemars(with = "[NonNegativeF32Schema; 3]"))]
    pub(super) color: [f32; 3],
    #[cfg_attr(feature = "schema", schemars(with = "NonNegativeF32Schema"))]
    pub(super) intensity: f32,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DirectionalLightWire {
    pub(super) direction_world: [f32; 3],
    #[cfg_attr(feature = "schema", schemars(with = "[NonNegativeF32Schema; 3]"))]
    pub(super) color: [f32; 3],
    #[cfg_attr(feature = "schema", schemars(with = "NonNegativeF32Schema"))]
    pub(super) illuminance_lux: f32,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PointLightWire {
    pub(super) position_world_m: [f32; 3],
    #[cfg_attr(feature = "schema", schemars(with = "[NonNegativeF32Schema; 3]"))]
    pub(super) color: [f32; 3],
    #[cfg_attr(feature = "schema", schemars(with = "NonNegativeF32Schema"))]
    pub(super) luminous_power_lm: f32,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RenderersWire {
    pub(super) wgpu: WgpuRendererWire,
    pub(super) blenderproc: BlenderProcRendererWire,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WgpuDevicePreferenceWire {
    Automatic,
    Cpu,
    Gpu,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WgpuAdapterPreferenceWire {
    Automatic,
    LowPower,
    HighPerformance,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WgpuRendererWire {
    pub(super) device_preference: WgpuDevicePreferenceWire,
    pub(super) adapter_preference: WgpuAdapterPreferenceWire,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveU32Schema"))]
    pub(super) shadow_map_size: u32,
    pub(super) rgb_fxaa: bool,
    /// Ambient-occlusion search radius in metres, about the size of the bin;
    /// 0 disables occlusion. Default 0.7.
    #[serde(default = "default_ambient_occlusion_radius_m")]
    #[cfg_attr(feature = "schema", schemars(range(min = 0.0)))]
    pub(super) ambient_occlusion_radius_m: f32,
    /// Glossy surfaces reflect what the camera sees (bin walls, other parts)
    /// before falling back to the environment. Default true.
    #[serde(default = "default_true")]
    pub(super) screen_space_reflections: bool,
}

const fn default_true() -> bool {
    true
}

const fn default_ambient_occlusion_radius_m() -> f32 {
    WgpuRendererConfig::DEFAULT_AMBIENT_OCCLUSION_RADIUS_M
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BlenderProcRendererWire {
    #[cfg_attr(feature = "schema", schemars(with = "NonBlankStringSchema"))]
    pub(super) executable_path: String,
    #[cfg_attr(feature = "schema", schemars(with = "NonBlankStringSchema"))]
    pub(super) worker_path: String,
    #[cfg_attr(feature = "schema", schemars(with = "NonBlankStringSchema"))]
    pub(super) device_selection: String,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveF32Schema"))]
    pub(super) handshake_timeout_s: f32,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveF32Schema"))]
    pub(super) request_timeout_s: f32,
    #[cfg_attr(feature = "schema", schemars(with = "PositiveU32Schema"))]
    pub(super) stderr_tail_capacity: u32,
    /// Maximum Cycles samples per pixel. Default 128.
    #[serde(default = "default_blenderproc_samples")]
    #[cfg_attr(feature = "schema", schemars(with = "PositiveU32Schema"))]
    pub(super) samples: u32,
    /// Adaptive-sampling noise threshold; 0 disables adaptive sampling. Default 0.01.
    #[serde(default = "default_blenderproc_noise_threshold")]
    #[cfg_attr(feature = "schema", schemars(range(min = 0.0)))]
    pub(super) noise_threshold: f32,
    /// RGB denoiser. Default `oidn`.
    #[serde(default = "default_blenderproc_denoiser")]
    pub(super) denoiser: BlenderProcDenoiserWire,
    /// Light bounces off glossy surfaces; 0 makes metals reflect only direct
    /// light. Default 1.
    #[serde(default = "default_blenderproc_glossy_bounces")]
    pub(super) glossy_bounces: u32,
}

const fn default_blenderproc_glossy_bounces() -> u32 {
    BlenderProcSampling::DEFAULT.glossy_bounces()
}

const fn default_blenderproc_samples() -> u32 {
    BlenderProcSampling::DEFAULT.samples()
}

const fn default_blenderproc_noise_threshold() -> f32 {
    BlenderProcSampling::DEFAULT.noise_threshold()
}

const fn default_blenderproc_denoiser() -> BlenderProcDenoiserWire {
    match BlenderProcSampling::DEFAULT.denoiser() {
        BlenderProcDenoiser::None => BlenderProcDenoiserWire::None,
        BlenderProcDenoiser::Oidn => BlenderProcDenoiserWire::Oidn,
        BlenderProcDenoiser::Optix => BlenderProcDenoiserWire::Optix,
    }
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum BlenderProcDenoiserWire {
    None,
    Oidn,
    Optix,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
/// At least one output channel must be enabled. Visible-instance output also
/// requires the resolved population maximum to fit in the instance-ID domain.
pub(super) struct OutputWire {
    pub(super) rgb: bool,
    pub(super) depth: bool,
    pub(super) visible_instances: bool,
    /// PNG compression for RGB and instance frames: `fast` (default),
    /// `balanced` or `high`. Pixels are identical at every level.
    #[serde(default)]
    pub(super) png_compression: PngCompressionWire,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PngCompressionWire {
    #[default]
    Fast,
    Balanced,
    High,
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum DistributionWire<T> {
    Fixed {
        value: T,
    },
    Uniform {
        min: T,
        max: T,
    },
    Choice {
        #[cfg_attr(feature = "schema", schemars(length(min = 1)))]
        values: Vec<WeightedValueWire<T>>,
    },
}

#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WeightedValueWire<T> {
    pub(super) value: T,
    pub(super) weight: f64,
}

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[schemars(transparent)]
struct PositiveU32Schema(#[schemars(range(min = 1))] u32);

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[schemars(transparent)]
struct PositiveF32Schema(#[schemars(extend("exclusiveMinimum" = 0.0))] f32);

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[schemars(transparent)]
struct NonNegativeF32Schema(#[schemars(range(min = 0.0))] f32);

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[schemars(transparent)]
struct NonNegativeF64Schema(#[schemars(range(min = 0.0))] f64);

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[schemars(transparent)]
struct NonBlankStringSchema(#[schemars(pattern(r"\S"))] String);

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[schemars(transparent)]
struct UnitF32Schema(#[schemars(range(min = 0.0, max = 1.0))] f32);

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[schemars(transparent)]
struct VerticalFovSchema(
    #[schemars(extend("exclusiveMinimum" = 0.0, "exclusiveMaximum" = 180.0))] f32,
);

/// Schema-only specialization of the actual generic distribution wire shape.
///
/// Uniform bounds must be ordered. Choice values must be nonempty and have a
/// finite, nonnegative weight set with positive total weight; those aggregate
/// constraints remain enforced by `RunConfig` validation.
#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DistributionSchema<T> {
    Fixed {
        value: T,
    },
    Uniform {
        min: T,
        max: T,
    },
    Choice {
        #[schemars(length(min = 1))]
        values: Vec<WeightedValueSchema<T>>,
    },
}

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WeightedValueSchema<T> {
    value: T,
    #[schemars(with = "NonNegativeF64Schema")]
    weight: f64,
}

/// Schema-only specialization for the runtime-supported asset mixture forms.
#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum AssetDistributionSchema {
    Fixed {
        #[schemars(with = "NonBlankStringSchema")]
        value: String,
    },
    Choice {
        #[schemars(length(min = 1))]
        values: Vec<AssetWeightedValueSchema>,
    },
}

#[cfg(feature = "schema")]
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AssetWeightedValueSchema {
    #[schemars(with = "NonBlankStringSchema")]
    value: String,
    #[schemars(with = "NonNegativeF64Schema")]
    weight: f64,
}

#[cfg(feature = "schema")]
pub(crate) fn json_schema() -> String {
    let mut schema = serde_json::to_value(schemars::schema_for!(RunConfigWire))
        .expect("serializing a generated JSON Schema cannot fail");
    remove_json_null(&mut schema);
    let mut json = serde_json::to_string_pretty(&schema)
        .expect("serializing a generated JSON Schema cannot fail");
    json.push('\n');
    json
}

#[cfg(feature = "schema")]
fn remove_json_null(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                remove_json_null(value);
            }
        }
        serde_json::Value::Object(object) => {
            let sole_type = if let Some(serde_json::Value::Array(types)) = object.get_mut("type") {
                types.retain(|value| value != "null");
                (types.len() == 1).then(|| types[0].clone())
            } else {
                None
            };
            if let Some(sole_type) = sole_type {
                object.insert("type".to_owned(), sole_type);
            }
            if let Some(serde_json::Value::Array(variants)) = object.get_mut("anyOf") {
                variants.retain(|variant| variant.get("type") != Some(&"null".into()));
            }
            for value in object.values_mut() {
                remove_json_null(value);
            }
        }
        _ => {}
    }
}

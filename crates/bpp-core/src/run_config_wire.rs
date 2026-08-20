use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RunConfigWire {
    pub(super) run: RunWire,
    pub(super) bin: BinWire,
    pub(super) assets: Vec<AssetWire>,
    pub(super) population: PopulationWire,
    pub(super) physics: PhysicsWire,
    pub(super) cameras: Vec<CameraWire>,
    pub(super) appearance: AppearanceWire,
    pub(super) renderers: RenderersWire,
    pub(super) output: OutputWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RunWire {
    pub(super) master_seed: u64,
    pub(super) valid_scene_count: u32,
    pub(super) scene_attempt_limit: Option<u32>,
    pub(super) placement_attempt_limit_per_object: Option<u32>,
    pub(super) simulation_workers: u32,
    pub(super) renderer_workers: u32,
    pub(super) queue_capacity: u32,
    pub(super) output_path: String,
    pub(super) cache_path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BinWire {
    pub(super) inner_dimensions_m: [f32; 3],
    pub(super) floor_thickness_m: f32,
    pub(super) wall_thickness_m: f32,
    pub(super) world_from_bin: TransformWire,
    pub(super) material_label: String,
    pub(super) physical_properties: SurfacePhysicalPropertiesWire,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SurfacePhysicalPropertiesWire {
    pub(super) friction: f32,
    pub(super) restitution: f32,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TransformWire {
    pub(super) translation: [f32; 3],
    pub(super) rotation_xyzw: [f32; 4],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AssetWire {
    pub(super) id: String,
    pub(super) source_path: String,
    pub(super) source_unit_scale_m: f32,
    pub(super) model_transform: TransformWire,
    pub(super) mixture_weight: f64,
    pub(super) physical_properties: PhysicalPropertyDistributionsWire,
    pub(super) visual_override: Option<PbrMaterialWire>,
    pub(super) vhacd: VhacdWire,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PhysicalPropertyDistributionsWire {
    pub(super) mass_kg: DistributionWire<f32>,
    pub(super) friction: DistributionWire<f32>,
    pub(super) restitution: DistributionWire<f32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PbrMaterialWire {
    pub(super) base_color_srgb: [f32; 4],
    pub(super) metallic: DistributionWire<f32>,
    pub(super) roughness: DistributionWire<f32>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VhacdWire {
    pub(super) resolution: u32,
    pub(super) max_convex_hulls: u32,
    pub(super) concavity: f32,
    pub(super) plane_downsampling: u32,
    pub(super) convex_hull_downsampling: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PopulationWire {
    pub(super) object_count: DistributionWire<u32>,
    pub(super) asset_mixture: DistributionWire<String>,
    pub(super) spawn_volume: SpawnVolumeWire,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SpawnVolumeWire {
    pub(super) min_m: [f32; 3],
    pub(super) max_m: [f32; 3],
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PhysicsWire {
    pub(super) fixed_timestep_s: Option<f32>,
    pub(super) substeps: u32,
    pub(super) solver_iterations: u32,
    pub(super) min_steps: Option<u32>,
    pub(super) max_steps: Option<u32>,
    pub(super) stability_check_interval: Option<u32>,
    pub(super) linear_sleep_threshold_mps: Option<f32>,
    pub(super) angular_sleep_threshold_rad_s: Option<f32>,
    pub(super) required_stable_checks: Option<u32>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CameraWire {
    pub(super) id: u16,
    pub(super) image_size: ImageSizeWire,
    pub(super) near_m: f32,
    pub(super) far_m: f32,
    pub(super) projection: CameraProjectionWire,
    pub(super) pose: CameraPoseWire,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ImageSizeWire {
    pub(super) width: u32,
    pub(super) height: u32,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CameraProjectionWire {
    pub(super) opencv_k: Option<[f32; 9]>,
    pub(super) vertical_fov_deg: Option<f32>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CameraPoseWire {
    pub(super) fixed_world_from_camera: Option<TransformWire>,
    pub(super) shell: Option<ShellCameraPoseWire>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ShellCameraPoseWire {
    pub(super) min_radius_m: f32,
    pub(super) max_radius_m: f32,
    pub(super) min_azimuth_deg: f32,
    pub(super) max_azimuth_deg: f32,
    pub(super) min_elevation_deg: f32,
    pub(super) max_elevation_deg: f32,
    pub(super) target_bin_m: [f32; 3],
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AppearanceWire {
    pub(super) background_srgb: [f32; 4],
    pub(super) ambient: AmbientWire,
    pub(super) directional_lights: Vec<DirectionalLightWire>,
    pub(super) point_lights: Vec<PointLightWire>,
    pub(super) material_defaults: PbrMaterialWire,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AmbientWire {
    pub(super) color: [f32; 3],
    pub(super) intensity: f32,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DirectionalLightWire {
    pub(super) direction_world: [f32; 3],
    pub(super) color: [f32; 3],
    pub(super) illuminance_lux: f32,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PointLightWire {
    pub(super) position_world_m: [f32; 3],
    pub(super) color: [f32; 3],
    pub(super) luminous_power_lm: f32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RenderersWire {
    pub(super) wgpu: WgpuRendererWire,
    pub(super) blenderproc: BlenderProcRendererWire,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WgpuDevicePreferenceWire {
    Automatic,
    Cpu,
    Gpu,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WgpuAdapterPreferenceWire {
    Automatic,
    LowPower,
    HighPerformance,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WgpuRendererWire {
    pub(super) device_preference: WgpuDevicePreferenceWire,
    pub(super) adapter_preference: WgpuAdapterPreferenceWire,
    pub(super) shadow_map_size: u32,
    pub(super) rgb_fxaa: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BlenderProcRendererWire {
    pub(super) executable_path: String,
    pub(super) worker_path: String,
    pub(super) device_selection: String,
    pub(super) handshake_timeout_s: f32,
    pub(super) request_timeout_s: f32,
    pub(super) stderr_tail_capacity: u32,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct OutputWire {
    pub(super) rgb: bool,
    pub(super) depth: bool,
    pub(super) visible_instances: bool,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum DistributionWire<T> {
    Fixed { value: T },
    Uniform { min: T, max: T },
    Choice { values: Vec<WeightedValueWire<T>> },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WeightedValueWire<T> {
    pub(super) value: T,
    pub(super) weight: f64,
}

//! Compact TOML case models for run-configuration properties.

use std::fmt::Write as _;

use proptest::prelude::*;

#[derive(Clone, Copy, Debug)]
pub enum ObjectCountCase {
    Fixed(u32),
    Uniform { minimum: u32, maximum: u32 },
    Choice { first: u32, second: u32 },
}

impl ObjectCountCase {
    pub fn maximum(self) -> u32 {
        match self {
            Self::Fixed(value) => value,
            Self::Uniform { maximum, .. } => maximum,
            Self::Choice { first, second } => first.max(second),
        }
    }

    fn toml(self) -> String {
        match self {
            Self::Fixed(value) => format!(r#"{{ kind = "fixed", value = {value} }}"#),
            Self::Uniform { minimum, maximum } => {
                format!(r#"{{ kind = "uniform", min = {minimum}, max = {maximum} }}"#)
            }
            Self::Choice { first, second } => format!(
                r#"{{ kind = "choice", values = [{{ value = {first}, weight = 1.0 }}, {{ value = {second}, weight = 2.0 }}] }}"#
            ),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PhysicsOverrides {
    pub fixed_timestep_s: f32,
    pub min_steps: u32,
    pub max_steps: u32,
    pub stability_check_interval: u32,
    pub linear_sleep_threshold_mps: f32,
    pub angular_sleep_threshold_rad_s: f32,
    pub required_stable_checks: u32,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct ValidConfigCase {
    pub master_seed: u64,
    pub bin_dimensions_m: [f32; 3],
    pub spawn_inset_steps: u8,
    pub asset_count: u8,
    pub reverse_assets: bool,
    pub positive_asset_weight: f64,
    pub other_asset_weight: f64,
    pub asset_mixture_is_choice: bool,
    pub object_count: ObjectCountCase,
    pub run_overrides: bool,
    pub physics_overrides: Option<PhysicsOverrides>,
    pub camera_start: u16,
    pub camera_count: u8,
    pub reverse_cameras: bool,
    pub projection_flip: bool,
    pub pose_flip: bool,
    pub output_rgb: bool,
    pub output_depth: bool,
    pub output_instances: bool,
}

impl ValidConfigCase {
    pub fn baseline() -> Self {
        Self {
            master_seed: 7,
            bin_dimensions_m: [4.0, 4.0, 4.0],
            spawn_inset_steps: 2,
            asset_count: 2,
            reverse_assets: false,
            positive_asset_weight: 1.0,
            other_asset_weight: 0.0,
            asset_mixture_is_choice: true,
            object_count: ObjectCountCase::Fixed(2),
            run_overrides: false,
            physics_overrides: None,
            camera_start: 1,
            camera_count: 2,
            reverse_cameras: false,
            projection_flip: false,
            pose_flip: false,
            output_rgb: true,
            output_depth: false,
            output_instances: true,
        }
    }

    pub fn asset_ids(&self) -> Vec<String> {
        (0..self.asset_count)
            .map(|index| format!("asset-{index:02}"))
            .collect()
    }

    pub fn camera_ids(&self) -> Vec<u16> {
        (0..u16::from(self.camera_count))
            .map(|offset| self.camera_start + offset)
            .collect()
    }

    pub fn spawn_bounds(&self) -> ([f32; 3], [f32; 3]) {
        let inset = f32::from(self.spawn_inset_steps) / 16.0;
        let [x, y, z] = self.bin_dimensions_m;
        (
            [-x / 2.0 + x * inset, -y / 2.0 + y * inset, z * inset],
            [x / 2.0 - x * inset, y / 2.0 - y * inset, z * (1.0 - inset)],
        )
    }

    pub fn camera_projection_is_fov(&self, id: u16) -> bool {
        let offset = id - self.camera_start;
        offset.is_multiple_of(2) ^ self.projection_flip
    }

    pub fn camera_pose_is_fixed(&self, id: u16) -> bool {
        let offset = id - self.camera_start;
        offset.is_multiple_of(2) ^ self.pose_flip
    }

    #[allow(clippy::too_many_lines)]
    pub fn toml(&self) -> String {
        let run_defaults = if self.run_overrides {
            "scene_attempt_limit = 7\nplacement_attempt_limit_per_object = 11\n"
        } else {
            ""
        };
        let [bin_x, bin_y, bin_z] = self.bin_dimensions_m;
        let (spawn_min, spawn_max) = self.spawn_bounds();

        let mut asset_indices = (0..self.asset_count).collect::<Vec<_>>();
        if self.reverse_assets {
            asset_indices.reverse();
        }
        let mut assets = String::new();
        for index in asset_indices {
            let id = format!("asset-{index:02}");
            let weight = if index == 0 {
                self.positive_asset_weight
            } else {
                self.other_asset_weight
            };
            let mass = match index % 3 {
                0 => r#"{ kind = "fixed", value = 0.5 }"#,
                1 => r#"{ kind = "uniform", min = 0.25, max = 1.0 }"#,
                _ => {
                    r#"{ kind = "choice", values = [{ value = 0.5, weight = 1.0 }, { value = 1.0, weight = 2.0 }] }"#
                }
            };
            let visual_override = if index % 2 == 0 {
                "visual_override = { base_color_srgb = [0.8, 0.2, 0.1, 1.0], metallic = { kind = \"fixed\", value = 0.1 }, roughness = { kind = \"uniform\", min = 0.2, max = 0.7 } }\n"
            } else {
                ""
            };
            write!(
                &mut assets,
                r#"
[[assets]]
id = "{id}"
source_path = "assets/{id}.glb"
source_unit_scale_m = 1.000000
model_transform = {{ translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }}
mixture_weight = {weight:.6}
physical_properties = {{ mass_kg = {mass}, friction = {{ kind = "fixed", value = 0.5 }}, restitution = {{ kind = "choice", values = [{{ value = 0.1, weight = 1.0 }}] }} }}
{visual_override}vhacd = {{ resolution = 1000, max_convex_hulls = 8, concavity = 0.0025, plane_downsampling = 2, convex_hull_downsampling = 2 }}
"#,
            )
            .expect("writing TOML to a String cannot fail");
        }

        let asset_ids = self.asset_ids();
        let asset_mixture = if self.asset_mixture_is_choice {
            let values = asset_ids
                .iter()
                .enumerate()
                .map(|(index, id)| {
                    format!(
                        r#"{{ value = "{id}", weight = {:.6} }}"#,
                        f64::from(u32::try_from(index).expect("asset strategy is bounded")) + 1.0
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!(r#"{{ kind = "choice", values = [{values}] }}"#)
        } else {
            format!(r#"{{ kind = "fixed", value = "{}" }}"#, asset_ids[0])
        };

        let physics = if let Some(physics) = self.physics_overrides {
            format!(
                "fixed_timestep_s = {:.6}\nsubsteps = 2\nsolver_iterations = 8\nmin_steps = {}\nmax_steps = {}\nstability_check_interval = {}\nlinear_sleep_threshold_mps = {:.6}\nangular_sleep_threshold_rad_s = {:.6}\nrequired_stable_checks = {}",
                physics.fixed_timestep_s,
                physics.min_steps,
                physics.max_steps,
                physics.stability_check_interval,
                physics.linear_sleep_threshold_mps,
                physics.angular_sleep_threshold_rad_s,
                physics.required_stable_checks,
            )
        } else {
            String::from("substeps = 2\nsolver_iterations = 8")
        };

        let mut camera_ids = self.camera_ids();
        if self.reverse_cameras {
            camera_ids.reverse();
        }
        let mut cameras = String::new();
        for id in camera_ids {
            let projection = if self.camera_projection_is_fov(id) {
                r"{ vertical_fov_deg = 55.0 }"
            } else {
                r"{ opencv_k = [500.0, 0.0, 320.0, 0.0, 500.0, 240.0, 0.0, 0.0, 1.0] }"
            };
            let pose = if self.camera_pose_is_fixed(id) {
                r"{ fixed_world_from_camera = { translation = [0.0, -1.0, 1.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] } }"
            } else {
                r"{ shell = { min_radius_m = 1.0, max_radius_m = 2.0, min_azimuth_deg = -90.0, max_azimuth_deg = 90.0, min_elevation_deg = 20.0, max_elevation_deg = 70.0, target_bin_m = [0.0, 0.0, 0.5] } }"
            };
            write!(
                &mut cameras,
                r"
[[cameras]]
id = {id}
image_size = {{ width = 640, height = 480 }}
near_m = 0.010000
far_m = 20.000000
projection = {projection}
pose = {pose}
",
            )
            .expect("writing TOML to a String cannot fail");
        }

        format!(
            r#"[run]
master_seed = {}
valid_scene_count = 3
{run_defaults}simulation_workers = 2
renderer_workers = 2
queue_capacity = 4
output_path = "output"
cache_path = "cache"

[bin]
inner_dimensions_m = [{bin_x:.6}, {bin_y:.6}, {bin_z:.6}]
floor_thickness_m = 0.020000
wall_thickness_m = 0.030000
world_from_bin = {{ translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }}
material_label = "steel"
physical_properties = {{ friction = 0.5, restitution = 0.1 }}
{assets}
[population]
object_count = {}
asset_mixture = {asset_mixture}
spawn_volume = {{ min_m = [{:.6}, {:.6}, {:.6}], max_m = [{:.6}, {:.6}, {:.6}] }}

[physics]
{physics}
{cameras}
[appearance]
background_srgb = [0.02, 0.03, 0.04, 1.0]
ambient = {{ color = [1.0, 1.0, 1.0], intensity = 0.15 }}
directional_lights = []
point_lights = []
material_defaults = {{ base_color_srgb = [0.7, 0.7, 0.7, 1.0], metallic = {{ kind = "fixed", value = 0.0 }}, roughness = {{ kind = "uniform", min = 0.35, max = 0.65 }} }}

[renderers.wgpu]
device_preference = "automatic"
adapter_preference = "high_performance"
shadow_map_size = 1024
rgb_fxaa = true

[renderers.blenderproc]
executable_path = "tools/blenderproc"
worker_path = "workers/render.py"
device_selection = "CPU"
handshake_timeout_s = 10.0
request_timeout_s = 30.0
stderr_tail_capacity = 1024

[output]
rgb = {}
depth = {}
visible_instances = {}
"#,
            self.master_seed,
            self.object_count.toml(),
            spawn_min[0],
            spawn_min[1],
            spawn_min[2],
            spawn_max[0],
            spawn_max[1],
            spawn_max[2],
            self.output_rgb,
            self.output_depth,
            self.output_instances,
        )
    }
}

fn object_count() -> BoxedStrategy<ObjectCountCase> {
    let maximum = u32::from(u16::MAX);
    prop_oneof![
        (1_u32..=maximum).prop_map(ObjectCountCase::Fixed),
        (1_u32..=maximum, 1_u32..=maximum).prop_map(|(left, right)| ObjectCountCase::Uniform {
            minimum: left.min(right),
            maximum: left.max(right),
        }),
        (1_u32..=maximum, 1_u32..=maximum)
            .prop_map(|(first, second)| ObjectCountCase::Choice { first, second }),
    ]
    .boxed()
}

fn physics_overrides() -> BoxedStrategy<Option<PhysicsOverrides>> {
    prop_oneof![
        Just(None),
        (
            1_u8..=10,
            1_u32..=128,
            1_u32..=16,
            1_u32..=8,
            0_u32..=128,
            0_u8..=20,
            0_u8..=20,
        )
            .prop_map(
                |(milliseconds, min_steps, interval, checks, slack, linear, angular)| {
                    let required_window = interval * checks;
                    let max_steps = min_steps.max(required_window) + slack;
                    Some(PhysicsOverrides {
                        fixed_timestep_s: f32::from(milliseconds) / 1_000.0,
                        min_steps,
                        max_steps,
                        stability_check_interval: interval,
                        linear_sleep_threshold_mps: f32::from(linear) / 1_000.0,
                        angular_sleep_threshold_rad_s: f32::from(angular) / 1_000.0,
                        required_stable_checks: checks,
                    })
                }
            ),
    ]
    .boxed()
}

fn enabled_outputs() -> BoxedStrategy<(bool, bool, bool)> {
    prop::sample::select(vec![
        (true, false, false),
        (false, true, false),
        (false, false, true),
        (true, true, false),
        (true, false, true),
        (false, true, true),
        (true, true, true),
    ])
    .boxed()
}

pub fn valid_config() -> BoxedStrategy<ValidConfigCase> {
    (
        (
            0_u64..=i64::MAX as u64,
            (4_u8..=16, 4_u8..=16, 4_u8..=16),
            1_u8..=3,
            1_u8..=3,
            any::<bool>(),
            1_u8..=16,
            0_u8..=16,
            any::<bool>(),
            object_count(),
        ),
        (
            any::<bool>(),
            physics_overrides(),
            0_u16..=1_000,
            1_u8..=3,
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            enabled_outputs(),
        ),
    )
        .prop_map(
            |(
                (
                    master_seed,
                    (bin_x, bin_y, bin_z),
                    spawn_inset_steps,
                    asset_count,
                    reverse_assets,
                    positive_weight,
                    other_weight,
                    asset_mixture_is_choice,
                    object_count,
                ),
                (
                    run_overrides,
                    physics_overrides,
                    camera_start,
                    camera_count,
                    reverse_cameras,
                    projection_flip,
                    pose_flip,
                    (output_rgb, output_depth, output_instances),
                ),
            )| ValidConfigCase {
                master_seed,
                bin_dimensions_m: [
                    f32::from(bin_x) / 4.0,
                    f32::from(bin_y) / 4.0,
                    f32::from(bin_z) / 4.0,
                ],
                spawn_inset_steps,
                asset_count,
                reverse_assets,
                positive_asset_weight: f64::from(positive_weight) / 4.0,
                other_asset_weight: f64::from(other_weight) / 4.0,
                asset_mixture_is_choice,
                object_count,
                run_overrides,
                physics_overrides,
                camera_start,
                camera_count,
                reverse_cameras,
                projection_flip,
                pose_flip,
                output_rgb,
                output_depth,
                output_instances,
            },
        )
        .boxed()
}

pub fn valid_large_population_without_instances() -> BoxedStrategy<ValidConfigCase> {
    (valid_config(), 65_536_u32..=70_000)
        .prop_map(|(mut case, count)| {
            case.object_count = ObjectCountCase::Fixed(count);
            case.output_rgb = true;
            case.output_depth = false;
            case.output_instances = false;
            case
        })
        .boxed()
}

#[derive(Clone, Copy, Debug)]
pub enum InvalidConfigCase {
    ZeroValidSceneCount,
    ZeroSimulationWorkers,
    DuplicateAssetId,
    DuplicateCameraId,
    UnknownAssetReference,
    AllZeroAssetWeights,
    ZeroObjectCount,
    ImpossibleObjectCount,
    SpawnInverted,
    SpawnOutsideBin,
    PhysicsOrdering,
    StabilityWindow,
    ProjectionBoth,
    PoseMissing,
    NoOutputChannels,
    VisibleInstanceOverflow,
    NestedUnknownField,
    WhitespaceOutputPath,
    WhitespaceAssetId,
    WhitespaceMaterialLabel,
    WhitespaceDeviceSelection,
}

impl InvalidConfigCase {
    pub const ALL: [Self; 21] = [
        Self::ZeroValidSceneCount,
        Self::ZeroSimulationWorkers,
        Self::DuplicateAssetId,
        Self::DuplicateCameraId,
        Self::UnknownAssetReference,
        Self::AllZeroAssetWeights,
        Self::ZeroObjectCount,
        Self::ImpossibleObjectCount,
        Self::SpawnInverted,
        Self::SpawnOutsideBin,
        Self::PhysicsOrdering,
        Self::StabilityWindow,
        Self::ProjectionBoth,
        Self::PoseMissing,
        Self::NoOutputChannels,
        Self::VisibleInstanceOverflow,
        Self::NestedUnknownField,
        Self::WhitespaceOutputPath,
        Self::WhitespaceAssetId,
        Self::WhitespaceMaterialLabel,
        Self::WhitespaceDeviceSelection,
    ];

    pub fn toml(self) -> String {
        let baseline = ValidConfigCase::baseline().toml();
        match self {
            Self::ZeroValidSceneCount => {
                replace_once(baseline, "valid_scene_count = 3", "valid_scene_count = 0")
            }
            Self::ZeroSimulationWorkers => {
                replace_once(baseline, "simulation_workers = 2", "simulation_workers = 0")
            }
            Self::DuplicateAssetId => {
                replace_once(baseline, "id = \"asset-01\"", "id = \"asset-00\"")
            }
            Self::DuplicateCameraId => replace_once(baseline, "id = 2\n", "id = 1\n"),
            Self::UnknownAssetReference => replace_once(
                baseline,
                r#"{ value = "asset-01", weight = 2.000000 }"#,
                r#"{ value = "missing", weight = 2.000000 }"#,
            ),
            Self::AllZeroAssetWeights => replace_once(
                baseline,
                "mixture_weight = 1.000000",
                "mixture_weight = 0.000000",
            ),
            Self::ZeroObjectCount => replace_once(
                baseline,
                r#"object_count = { kind = "fixed", value = 2 }"#,
                r#"object_count = { kind = "fixed", value = 0 }"#,
            ),
            Self::ImpossibleObjectCount => replace_once(
                baseline,
                r#"object_count = { kind = "fixed", value = 2 }"#,
                r#"object_count = { kind = "uniform", min = 5, max = 2 }"#,
            ),
            Self::SpawnInverted => replace_once(
                baseline,
                "spawn_volume = { min_m = [-1.500000, -1.500000, 0.500000], max_m = [1.500000, 1.500000, 3.500000] }",
                "spawn_volume = { min_m = [0.500000, -1.500000, 0.500000], max_m = [0.500000, 1.500000, 3.500000] }",
            ),
            Self::SpawnOutsideBin => replace_once(
                baseline,
                "spawn_volume = { min_m = [-1.500000, -1.500000, 0.500000], max_m = [1.500000, 1.500000, 3.500000] }",
                "spawn_volume = { min_m = [-2.500000, -1.500000, 0.500000], max_m = [1.500000, 1.500000, 3.500000] }",
            ),
            Self::PhysicsOrdering => replace_once(
                baseline,
                "solver_iterations = 8",
                "solver_iterations = 8\nmin_steps = 100\nmax_steps = 10",
            ),
            Self::StabilityWindow => replace_once(
                baseline,
                "solver_iterations = 8",
                "solver_iterations = 8\nmin_steps = 1\nmax_steps = 20\nstability_check_interval = 8\nrequired_stable_checks = 3",
            ),
            Self::ProjectionBoth => replace_once(
                baseline,
                r"projection = { vertical_fov_deg = 55.0 }",
                r"projection = { vertical_fov_deg = 55.0, opencv_k = [500.0, 0.0, 320.0, 0.0, 500.0, 240.0, 0.0, 0.0, 1.0] }",
            ),
            Self::PoseMissing => replace_once(
                baseline,
                r"pose = { fixed_world_from_camera = { translation = [0.0, -1.0, 1.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] } }",
                "pose = {}",
            ),
            Self::NoOutputChannels => {
                let without_rgb =
                    replace_once(baseline, "[output]\nrgb = true", "[output]\nrgb = false");
                replace_once(
                    without_rgb,
                    "visible_instances = true",
                    "visible_instances = false",
                )
            }
            Self::VisibleInstanceOverflow => replace_once(
                baseline,
                r#"object_count = { kind = "fixed", value = 2 }"#,
                r#"object_count = { kind = "fixed", value = 65536 }"#,
            ),
            Self::NestedUnknownField => replace_once(
                baseline,
                "id = 1\nimage_size = { width = 640, height = 480 }",
                "id = 1\nimage_size = { width = 640, height = 480, bogus = 1 }",
            ),
            Self::WhitespaceOutputPath => {
                replace_once(baseline, "output_path = \"output\"", "output_path = \"  \"")
            }
            Self::WhitespaceAssetId => replace_once(baseline, "id = \"asset-00\"", "id = \"  \""),
            Self::WhitespaceMaterialLabel => replace_once(
                baseline,
                "material_label = \"steel\"",
                "material_label = \"  \"",
            ),
            Self::WhitespaceDeviceSelection => replace_once(
                baseline,
                "device_selection = \"CPU\"",
                "device_selection = \"  \"",
            ),
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn replace_once(input: String, needle: &str, replacement: &str) -> String {
    assert_eq!(
        input.match_indices(needle).count(),
        1,
        "invalid-case marker must be unique: {needle:?}",
    );
    input.replacen(needle, replacement, 1)
}

pub fn invalid_config() -> BoxedStrategy<InvalidConfigCase> {
    prop::sample::select(InvalidConfigCase::ALL.to_vec()).boxed()
}

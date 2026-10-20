//! End-to-end cache and geometric acceptance for convex preprocessing.

use std::{
    cell::Cell,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Barrier,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::Instant,
};

use bop_assets::{
    AssetCacheKeyInput, AssetSourceFormat, CacheDisposition, ConvexCacheIoOperation,
    ConvexDecompositionCache, ConvexDecompositionCacheError, ConvexPart, GlbLoadLimits,
    asset_cache_key, canonicalize_visual_asset, generate_convex_parts, load_glb,
};
use bop_core::{PositiveMeters, RigidTransform, RunConfig, VhacdParameters};
use parry3d::{math::Vector, query::PointQuery, shape::ConvexPolyhedron};

const CUBE_INDICES: [u32; 36] = [
    0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 3, 7, 6, 3, 6, 2, 0, 4, 7, 0, 7, 3, 1, 2,
    6, 1, 6, 5,
];
const CUBE_POSITIONS: [[f32; 3]; 8] = [
    [-0.5, -0.5, -0.5],
    [0.5, -0.5, -0.5],
    [0.5, 0.5, -0.5],
    [-0.5, 0.5, -0.5],
    [-0.5, -0.5, 0.5],
    [0.5, -0.5, 0.5],
    [0.5, 0.5, 0.5],
    [-0.5, 0.5, 0.5],
];

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bop-assets-{label}-{}-{serial}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("unique test directory is created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn append_f32(bytes: &mut Vec<u8>, values: impl IntoIterator<Item = f32>) {
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
}

fn cube_glb() -> Vec<u8> {
    let mut binary = Vec::new();
    append_f32(&mut binary, CUBE_POSITIONS.iter().flatten().copied());
    let normal_offset = binary.len();
    append_f32(
        &mut binary,
        std::iter::repeat_n([0.0_f32, 0.0, 1.0], CUBE_POSITIONS.len()).flatten(),
    );
    let index_offset = binary.len();
    for index in CUBE_INDICES {
        binary.extend_from_slice(&index.to_le_bytes());
    }
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"buffers":[{{"byteLength":{}}}],"bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{normal_offset}}},{{"buffer":0,"byteOffset":{normal_offset},"byteLength":{}}},{{"buffer":0,"byteOffset":{index_offset},"byteLength":{}}}],"accessors":[{{"bufferView":0,"componentType":5126,"count":8,"type":"VEC3","min":[-0.5,-0.5,-0.5],"max":[0.5,0.5,0.5]}},{{"bufferView":1,"componentType":5126,"count":8,"type":"VEC3"}},{{"bufferView":2,"componentType":5125,"count":36,"type":"SCALAR"}}],"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0,"NORMAL":1}},"indices":2,"mode":4}}]}}],"nodes":[{{"mesh":0}}],"scenes":[{{"nodes":[0]}}],"scene":0}}"#,
        binary.len(),
        index_offset - normal_offset,
        binary.len() - index_offset,
    );
    pack_glb(&json, &binary)
}

fn pack_glb(json: &str, binary: &[u8]) -> Vec<u8> {
    let mut json = json.as_bytes().to_vec();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut binary = binary.to_vec();
    while !binary.len().is_multiple_of(4) {
        binary.push(0);
    }
    let total = 12 + 8 + json.len() + 8 + binary.len();
    let mut bytes = Vec::with_capacity(total);
    bytes.extend_from_slice(b"glTF");
    bytes.extend_from_slice(&2_u32.to_le_bytes());
    bytes.extend_from_slice(
        &u32::try_from(total)
            .expect("fixture length fits u32")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(json.len())
            .expect("fixture JSON length fits u32")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&0x4e4f_534a_u32.to_le_bytes());
    bytes.extend_from_slice(&json);
    bytes.extend_from_slice(
        &u32::try_from(binary.len())
            .expect("fixture BIN length fits u32")
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&0x004e_4942_u32.to_le_bytes());
    bytes.extend_from_slice(&binary);
    bytes
}

fn parameters() -> VhacdParameters {
    let mut input = String::from(
        r#"[run]
master_seed = 1
valid_scene_count = 1
simulation_workers = 1
renderer_workers = 1
queue_capacity = 1
output_path = "output"
cache_path = "cache"
[bin]
inner_dimensions_m = [2.0, 2.0, 2.0]
floor_thickness_m = 0.02
wall_thickness_m = 0.03
world_from_bin = { translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }
material_label = "steel"
physical_properties = { friction = 0.5, restitution = 0.1 }
[[assets]]
id = "cube"
source_path = "cube.glb"
source_unit_scale_m = 1.0
model_transform = { translation = [0.0, 0.0, 0.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] }
mixture_weight = 1.0
physical_properties = { mass_kg = { kind = "fixed", value = 0.5 }, friction = { kind = "fixed", value = 0.5 }, restitution = { kind = "fixed", value = 0.1 } }
"#,
    );
    writeln!(
        input,
        "vhacd = {{ resolution = 10000, max_convex_hulls = 8, concavity = 0.0025, plane_downsampling = 4, convex_hull_downsampling = 4 }}"
    )
    .expect("writing String succeeds");
    input.push_str(
        r#"[population]
object_count = { kind = "fixed", value = 1 }
asset_mixture = { kind = "fixed", value = "cube" }
spawn_volume = { min_m = [-0.5, -0.5, 0.0], max_m = [0.5, 0.5, 1.0] }
[physics]
substeps = 1
solver_iterations = 1
[[cameras]]
id = 1
image_size = { width = 16, height = 16 }
near_m = 0.01
far_m = 10.0
projection = { vertical_fov_deg = 55.0 }
pose = { fixed_world_from_camera = { translation = [0.0, -1.0, 1.0], rotation_xyzw = [0.0, 0.0, 0.0, 1.0] } }
[appearance]
background_srgb = [0.0, 0.0, 0.0, 1.0]
ambient = { color = [1.0, 1.0, 1.0], intensity = 0.0 }
directional_lights = []
point_lights = []
material_defaults = { base_color_srgb = [0.5, 0.5, 0.5, 1.0], metallic = { kind = "fixed", value = 0.0 }, roughness = { kind = "fixed", value = 0.5 } }
[renderers.wgpu]
device_preference = "automatic"
adapter_preference = "high_performance"
shadow_map_size = 16
rgb_fxaa = false
[renderers.blenderproc]
executable_path = "blenderproc"
worker_path = "worker.py"
device_selection = "CPU"
handshake_timeout_s = 1.0
request_timeout_s = 1.0
stderr_tail_capacity = 64
[output]
rgb = true
depth = false
visible_instances = false
"#,
    );
    RunConfig::from_toml(&input)
        .expect("cache fixture config parses")
        .plan()
        .assets()[0]
        .vhacd()
}

fn transform() -> RigidTransform {
    RigidTransform::try_new(
        [2.0, 3.0, 4.0],
        [
            0.0,
            0.0,
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        ],
    )
    .expect("fixture transform is valid")
}

fn fixture() -> (Vec<u8>, bop_assets::CanonicalVisualAsset, VhacdParameters) {
    let source = cube_glb();
    let limits =
        GlbLoadLimits::try_new(16, 4, 32, 64, 16, 1024, 4096).expect("fixture limits are nonzero");
    let loaded = load_glb(&source, limits).expect("cube fixture loads");
    let canonical =
        canonicalize_visual_asset(loaded, transform()).expect("cube fixture canonicalizes");
    (source, canonical, parameters())
}

fn key(source: &[u8], parameters: VhacdParameters) -> bop_assets::AssetCacheKey {
    asset_cache_key(AssetCacheKeyInput::new(
        source,
        AssetSourceFormat::Glb,
        PositiveMeters::try_new(1.0).expect("one metre is positive"),
        transform(),
        parameters,
    ))
}

fn only_entry(root: &Path) -> PathBuf {
    let version = fs::read_dir(root.join("convex-v1"))
        .expect("cache version directory exists")
        .next()
        .expect("one prefix directory exists")
        .expect("prefix entry is readable")
        .path();
    fs::read_dir(version)
        .expect("prefix directory is readable")
        .next()
        .expect("one cache entry exists")
        .expect("cache entry is readable")
        .path()
}

fn tetrahedron() -> ConvexPart {
    ConvexPart::try_new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ],
        vec![0, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3],
    )
    .expect("fixture tetrahedron is valid")
}

fn temporary_entries(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut temporary = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).expect("cache directory is readable") {
            let path = entry.expect("cache entry is readable").path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().contains(".tmp-"))
            {
                temporary.push(path);
            }
        }
    }
    temporary
}

fn voxel_axis_resolution(voxel_budget: u32) -> u16 {
    (2_u16..=1_626)
        .find(|resolution| u64::from(*resolution).pow(3) >= u64::from(voxel_budget))
        .expect("accepted voxel budget has a representable cube root")
}

#[test]
fn first_lookup_misses_and_second_lookup_hits_without_generation() {
    let directory = TestDirectory::new("hit-miss");
    let cache = ConvexDecompositionCache::new(directory.path());
    let (source, asset, parameters) = fixture();
    let key = key(&source, parameters);
    let calls = Cell::new(0_u32);

    let first = cache
        .get_or_generate(key, || {
            calls.set(calls.get() + 1);
            generate_convex_parts(&asset, parameters)
        })
        .expect("cold cache generation succeeds");
    assert_eq!(first.disposition(), CacheDisposition::Miss);
    assert_eq!(calls.get(), 1);

    let second = cache
        .get_or_generate(key, || panic!("warm hit must not invoke V-HACD"))
        .expect("warm cache lookup succeeds");
    assert_eq!(second.disposition(), CacheDisposition::Hit);
    assert_eq!(second.parts(), first.parts());
    assert_eq!(calls.get(), 1);
}

#[test]
fn checksum_corruption_is_regenerated_and_atomically_replaced() {
    let directory = TestDirectory::new("corruption");
    let cache = ConvexDecompositionCache::new(directory.path());
    let (source, asset, parameters) = fixture();
    let key = key(&source, parameters);
    let original = cache
        .get_or_generate(key, || generate_convex_parts(&asset, parameters))
        .expect("initial generation succeeds");
    let entry = only_entry(directory.path());
    let mut bytes = fs::read(&entry).expect("cache entry is readable");
    let midpoint = bytes.len() / 2;
    bytes[midpoint] ^= 0x80;
    fs::write(&entry, bytes).expect("test corrupts cache entry");

    let calls = Cell::new(0_u32);
    let rebuilt = cache
        .get_or_generate(key, || {
            calls.set(calls.get() + 1);
            generate_convex_parts(&asset, parameters)
        })
        .expect("corrupt entry regenerates");
    assert_eq!(rebuilt.disposition(), CacheDisposition::Miss);
    assert_eq!(calls.get(), 1);
    assert_eq!(rebuilt.parts(), original.parts());

    let hit = cache
        .get_or_generate(key, || panic!("replacement must be readable"))
        .expect("replacement is a hit");
    assert_eq!(hit.disposition(), CacheDisposition::Hit);
}

#[test]
fn maximum_part_count_round_trips() {
    let directory = TestDirectory::new("maximum-parts");
    let cache = ConvexDecompositionCache::new(directory.path());
    let (source, _, parameters) = fixture();
    let key = key(&source, parameters);
    let expected = vec![tetrahedron(); 1_024];

    let miss = cache
        .get_or_generate(key, || Ok(expected.clone()))
        .expect("maximum supported part count is writable");
    assert_eq!(miss.disposition(), CacheDisposition::Miss);

    let hit = cache
        .get_or_generate(key, || panic!("maximum-size entry must hit"))
        .expect("maximum supported part count is readable");
    assert_eq!(hit.disposition(), CacheDisposition::Hit);
    assert_eq!(hit.parts(), expected);
}

#[test]
fn part_count_above_maximum_is_rejected_before_write() {
    let directory = TestDirectory::new("too-many-parts");
    let cache = ConvexDecompositionCache::new(directory.path());
    let (source, _, parameters) = fixture();
    let key = key(&source, parameters);

    let error = cache
        .get_or_generate(key, || Ok(vec![tetrahedron(); 1_025]))
        .unwrap_err();
    assert!(matches!(
        error,
        ConvexDecompositionCacheError::PartLimitExceeded {
            actual: 1_025,
            maximum: 1_024
        }
    ));
    assert!(
        !directory.path().join("convex-v1").exists(),
        "rejected output must not create a cache entry"
    );
}

#[test]
fn concurrent_rebuild_of_corrupt_entry_leaves_one_readable_target() {
    const CALLERS: usize = 8;

    let directory = TestDirectory::new("concurrent-corruption");
    let cache = ConvexDecompositionCache::new(directory.path());
    let (source, _, parameters) = fixture();
    let key = key(&source, parameters);
    let expected = vec![tetrahedron()];
    cache
        .get_or_generate(key, || Ok(expected.clone()))
        .expect("initial entry is published");
    let entry = only_entry(directory.path());
    fs::write(&entry, b"corrupt").expect("entry is deliberately corrupted");

    let barrier = Arc::new(Barrier::new(CALLERS));
    let handles: Vec<_> = (0..CALLERS)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let cache = cache.clone();
            let expected = expected.clone();
            thread::spawn(move || {
                barrier.wait();
                cache.get_or_generate(key, || Ok(expected))
            })
        })
        .collect();

    for handle in handles {
        let rebuilt = handle
            .join()
            .expect("cache caller does not panic")
            .expect("concurrent publication succeeds");
        assert_eq!(rebuilt.parts(), expected);
    }
    let hit = cache
        .get_or_generate(key, || panic!("published entry must be readable"))
        .expect("one valid target remains");
    assert_eq!(hit.disposition(), CacheDisposition::Hit);
    assert_eq!(hit.parts(), expected);
    assert_eq!(temporary_entries(directory.path()), Vec::<PathBuf>::new());
}

#[test]
fn canonical_transform_is_applied_exactly_once_before_decomposition() {
    let (_, asset, _) = fixture();
    let positions = asset.visual_mesh().primitives()[0].positions();

    assert!(positions.contains(&[2.5, 2.5, 3.5]));
    assert!(positions.contains(&[1.5, 3.5, 4.5]));
    assert!(!positions.contains(&[-0.5, -0.5, -0.5]));
}

#[test]
fn every_canonical_cube_vertex_is_contained_by_a_generated_hull() {
    let (_, asset, parameters) = fixture();
    let parts = generate_convex_parts(&asset, parameters).expect("cube decomposition succeeds");
    let axis_resolution = voxel_axis_resolution(parameters.resolution());
    let voxel_size_m = 1.0 / f32::from(axis_resolution - 1);
    let tolerance_m = 0.5 * 3.0_f32.sqrt() * voxel_size_m + 1.0e-5;
    let hulls: Vec<_> = parts
        .iter()
        .map(|part| {
            let vertices: Vec<_> = part
                .vertices()
                .iter()
                .copied()
                .map(Vector::from_array)
                .collect();
            ConvexPolyhedron::from_convex_hull(&vertices).expect("generated part is volumetric")
        })
        .collect();

    for position in asset.visual_mesh().primitives()[0].positions() {
        let point = Vector::from_array(*position);
        let minimum_distance = hulls
            .iter()
            .map(|hull| hull.distance_to_local_point(point, true))
            .fold(f32::INFINITY, f32::min);
        assert!(
            minimum_distance <= tolerance_m,
            "canonical vertex {position:?} is outside every collision hull: distance={minimum_distance}"
        );
    }
}

#[test]
fn directory_failures_remain_typed_hard_errors() {
    let directory = TestDirectory::new("io-error");
    let root_file = directory.path().join("not-a-directory");
    fs::write(&root_file, b"file").expect("blocking file is created");
    let cache = ConvexDecompositionCache::new(&root_file);
    let (source, asset, parameters) = fixture();
    let error = cache
        .get_or_generate(key(&source, parameters), || {
            generate_convex_parts(&asset, parameters)
        })
        .unwrap_err();

    assert!(
        matches!(
            &error,
            ConvexDecompositionCacheError::Io {
                operation: ConvexCacheIoOperation::Read,
                ..
            }
        ),
        "unexpected error: {error:?}"
    );
}

#[test]
#[ignore = "manual non-gating cold/warm cache timing"]
fn measure_controlled_cube_cold_and_warm_cache() {
    let directory = TestDirectory::new("timing");
    let cache = ConvexDecompositionCache::new(directory.path());
    let (source, asset, parameters) = fixture();
    let key = key(&source, parameters);

    let cold_start = Instant::now();
    let cold = cache
        .get_or_generate(key, || generate_convex_parts(&asset, parameters))
        .expect("cold generation succeeds");
    let cold_elapsed = cold_start.elapsed();
    let warm_start = Instant::now();
    let warm = cache
        .get_or_generate(key, || panic!("warm timing must not generate"))
        .expect("warm lookup succeeds");
    let warm_elapsed = warm_start.elapsed();

    assert_eq!(cold.disposition(), CacheDisposition::Miss);
    assert_eq!(warm.disposition(), CacheDisposition::Hit);
    assert_eq!(warm.parts(), cold.parts());
    eprintln!("controlled cube cache: cold={cold_elapsed:?}, warm={warm_elapsed:?}");
}

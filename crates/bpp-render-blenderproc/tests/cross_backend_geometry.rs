//! Cross-backend geometric acceptance metrics and hardware gate.

use std::{fs::File, io::BufReader, path::Path, sync::Arc};

use bpp_assets::{
    ConvexPart, NormalizedAsset, ResolvedAsset, ResolvedAssetCatalog, VisualMesh, VisualPrimitive,
};
use bpp_core::{
    AssetId, BlenderProcRendererConfig, CameraId, RenderSceneRequest, Renderer,
    WgpuAdapterPreference, WgpuDevicePreference, WgpuRendererConfig,
};
use bpp_dataset::SceneStagingArea;
use bpp_render_blenderproc::BlenderProcRenderer;
use bpp_render_wgpu::WgpuRenderer;
use exr::prelude::{ReadChannels as _, ReadLayers as _, ReadSpecificChannel as _, read};

const REQUEST: &str = r#"{"schema_version":1,"scene":{"schema_version":1,"scene_id":7,"accepted_attempt":0,"seed_fingerprint":"seed","bin":{"schema_version":1,"inner_dimensions_m":[1.0,1.0,1.0],"floor_thickness_m":0.1,"wall_thickness_m":0.1,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel"},"instances":[{"schema_version":1,"instance_id":1,"asset_id":"triangle","world_from_model":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"physical_properties":{"schema_version":1,"mass_kg":1.0,"friction":0.5,"restitution":0.0}}],"physics":{"schema_version":1,"attempts":1,"steps":1,"simulated_seconds":0.01,"final_max_linear_speed_mps":0.0,"final_max_angular_speed_rad_s":0.0,"final_max_penetration_m":0.0,"rejection_counts":[]}},"cameras":[{"schema_version":1,"camera_id":0,"image_size":{"width":64,"height":64},"k":[50.0,0.0,32.0,0.0,50.0,32.0,0.0,0.0,1.0],"camera_from_world":{"camera_from_world":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"near_m":0.1,"far_m":10.0}],"appearance":{"schema_version":1,"background_srgb":[0.0,0.0,0.0,1.0],"ambient_light":{"schema_version":1,"color":[1.0,1.0,1.0],"intensity":1.0},"directional_lights":[],"point_lights":[]},"required_channels":{"schema_version":1,"rgb":true,"depth":true,"instance":true}}"#;

fn erode_one_pixel(mask: &[bool], width: usize, height: usize) -> Vec<bool> {
    let mut eroded = vec![false; mask.len()];
    if width < 3 || height < 3 || width.checked_mul(height) != Some(mask.len()) {
        return eroded;
    }
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            eroded[y * width + x] = (y - 1..=y + 1).all(|neighbor_y| {
                (x - 1..=x + 1).all(|neighbor_x| mask[neighbor_y * width + neighbor_x])
            });
        }
    }
    eroded
}

fn nearest_rank(values: &mut [f32], numerator: usize, denominator: usize) -> f32 {
    assert!(!values.is_empty());
    assert!(numerator > 0 && numerator <= denominator);
    values.sort_by(f32::total_cmp);
    let rank = values.len().saturating_mul(numerator).div_ceil(denominator);
    values[rank - 1]
}

fn mask_bounds(mask: &[bool], width: usize) -> Option<([usize; 2], [usize; 2])> {
    let mut minimum = [usize::MAX; 2];
    let mut maximum = [0; 2];
    let mut found = false;
    for (index, foreground) in mask.iter().copied().enumerate() {
        if foreground {
            let point = [index % width, index / width];
            minimum[0] = minimum[0].min(point[0]);
            minimum[1] = minimum[1].min(point[1]);
            maximum[0] = maximum[0].max(point[0]);
            maximum[1] = maximum[1].max(point[1]);
            found = true;
        }
    }
    found.then_some((minimum, maximum))
}

fn one_pixel_tolerant_iou(left: &[bool], right: &[bool], width: usize, height: usize) -> f64 {
    assert_eq!(left.len(), right.len());
    assert_eq!(width.checked_mul(height), Some(left.len()));

    let matched = |source: &[bool], target: &[bool]| {
        source
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, foreground)| *foreground)
            .filter(|(index, _)| {
                let x = index % width;
                let y = index / width;
                let min_x = x.saturating_sub(1);
                let max_x = (x + 1).min(width.saturating_sub(1));
                let min_y = y.saturating_sub(1);
                let max_y = (y + 1).min(height.saturating_sub(1));
                (min_y..=max_y).any(|neighbor_y| {
                    (min_x..=max_x).any(|neighbor_x| target[neighbor_y * width + neighbor_x])
                })
            })
            .count()
    };
    let foreground = u32::try_from(
        left.iter().filter(|value| **value).count() + right.iter().filter(|value| **value).count(),
    )
    .unwrap();
    assert!(foreground > 0);
    let symmetric_matches = u32::try_from(matched(left, right) + matched(right, left)).unwrap();
    let tolerant_intersection = f64::from(symmetric_matches) / 2.0;
    let tolerant_union = f64::from(foreground) - tolerant_intersection;
    tolerant_intersection / tolerant_union
}

fn catalog() -> ResolvedAssetCatalog {
    let primitive = VisualPrimitive::try_new(
        vec![[-0.6, -0.6, 2.0], [0.6, -0.6, 2.0], [0.0, 0.6, 2.0]],
        vec![[0.0, 0.0, -1.0]; 3],
        vec![],
        vec![0, 2, 1],
        None,
    )
    .unwrap();
    let collision = ConvexPart::try_new(
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
        ],
        vec![0, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3],
    )
    .unwrap();
    let asset = Arc::new(
        NormalizedAsset::try_new(
            VisualMesh::try_new(vec![primitive]).unwrap(),
            vec![],
            vec![],
            vec![collision],
        )
        .unwrap(),
    );
    ResolvedAssetCatalog::try_new(vec![ResolvedAsset::new(
        AssetId::new("triangle").unwrap(),
        *blake3::hash(b"cross-backend-triangle").as_bytes(),
        asset,
    )])
    .unwrap()
}

fn staging(root: &Path, request: &RenderSceneRequest) -> SceneStagingArea {
    let staging = SceneStagingArea::create(root, request.scene().scene_id()).unwrap();
    std::fs::write(
        staging.scene_metadata_path(),
        serde_json::to_vec(request.scene()).unwrap(),
    )
    .unwrap();
    for camera in request.cameras() {
        staging.prepare_camera(camera.camera_id()).unwrap();
        std::fs::write(
            staging.frame_metadata_path(camera.camera_id()),
            serde_json::to_vec(camera).unwrap(),
        )
        .unwrap();
    }
    staging
}

fn read_instance_mask(path: &Path) -> Vec<bool> {
    let mut reader = png::Decoder::new(BufReader::new(File::open(path).unwrap()))
        .read_info()
        .unwrap();
    let mut bytes = vec![0; reader.output_buffer_size().unwrap()];
    let output = reader.next_frame(&mut bytes).unwrap();
    bytes[..output.buffer_size()]
        .chunks_exact(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]) != 0)
        .collect()
}

fn read_depth(path: &Path, width: usize) -> Vec<f32> {
    read()
        .no_deep_data()
        .largest_resolution_level()
        .specific_channels()
        .required("Z")
        .collect_pixels(
            |resolution, _| vec![0.0_f32; resolution.area()],
            move |samples, position, (value,): (f32,)| {
                samples[position.y() * width + position.x()] = value;
            },
        )
        .first_valid_layer()
        .all_attributes()
        .from_file(path)
        .unwrap()
        .layer_data
        .channel_data
        .pixels
}

#[test]
fn one_pixel_erosion_removes_mask_boundaries() {
    let mask = vec![
        false, false, false, false, false, false, true, true, true, false, false, true, true, true,
        false, false, true, true, true, false, false, false, false, false, false,
    ];

    assert_eq!(
        erode_one_pixel(&mask, 5, 5),
        vec![
            false, false, false, false, false, false, false, false, false, false, false, false,
            true, false, false, false, false, false, false, false, false, false, false, false,
            false,
        ]
    );
}

#[test]
fn percentile_uses_nearest_rank_for_acceptance_thresholds() {
    let mut values = vec![0.005, 0.001, 0.003, 0.002, 0.004];
    assert!((nearest_rank(&mut values, 1, 2) - 0.003).abs() <= f32::EPSILON);
    assert!((nearest_rank(&mut values, 99, 100) - 0.005).abs() <= f32::EPSILON);
}

#[test]
fn tolerant_iou_accepts_a_one_pixel_shift_but_not_a_two_pixel_gap() {
    let left = vec![false, true, false, false];
    let one_pixel_right = vec![false, false, true, false];
    let two_pixels_right = vec![false, false, false, true];

    assert!((one_pixel_tolerant_iou(&left, &one_pixel_right, 4, 1) - 1.0).abs() <= f64::EPSILON);
    assert!(one_pixel_tolerant_iou(&left, &two_pixels_right, 4, 1).abs() <= f64::EPSILON);
}

#[test]
#[ignore = "requires Metal and the pinned BlenderProc/Blender runtime"]
fn analytic_triangle_matches_wgpu_geometry_thresholds() {
    let request: RenderSceneRequest = serde_json::from_str(REQUEST).unwrap();
    let catalog = catalog();
    let roots = tempfile::tempdir().unwrap();
    let wgpu_root = roots.path().join("wgpu");
    let blender_root = roots.path().join("blenderproc");
    std::fs::create_dir_all(&wgpu_root).unwrap();
    std::fs::create_dir_all(&blender_root).unwrap();
    let wgpu_staging = staging(&wgpu_root, &request);
    let blender_staging = staging(&blender_root, &request);

    let wgpu_config = WgpuRendererConfig::try_new(
        WgpuDevicePreference::Automatic,
        WgpuAdapterPreference::HighPerformance,
        16,
        false,
    )
    .unwrap();
    let mut wgpu = WgpuRenderer::try_new(wgpu_config).unwrap();
    wgpu.prepare(&catalog).unwrap();
    wgpu.render_scene(&request, &wgpu_staging).unwrap();

    let executable =
        std::env::var_os("BPP_BLENDERPROC_EXECUTABLE").unwrap_or_else(|| "blenderproc".into());
    let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/blenderproc-worker/blenderproc_worker.py");
    let config =
        BlenderProcRendererConfig::try_new(executable, worker, "CPU", 60.0, 300.0, 65_536).unwrap();
    let mut blender = BlenderProcRenderer::try_new(config).unwrap();
    blender.prepare(&catalog).unwrap();
    blender.render_scene(&request, &blender_staging).unwrap();

    bpp_dataset::validate_and_write_checksums(&wgpu_staging, &request).unwrap();
    bpp_dataset::validate_and_write_checksums(&blender_staging, &request).unwrap();

    let camera = CameraId::new(0);
    let wgpu_mask = read_instance_mask(&wgpu_staging.instance_path(camera));
    let blender_mask = read_instance_mask(&blender_staging.instance_path(camera));
    let wgpu_depth = read_depth(&wgpu_staging.depth_path(camera), 64);
    let blender_depth = read_depth(&blender_staging.depth_path(camera), 64);
    let wgpu_eroded = erode_one_pixel(&wgpu_mask, 64, 64);
    let blender_eroded = erode_one_pixel(&blender_mask, 64, 64);
    eprintln!(
        "wgpu foreground={} bounds={:?}; BlenderProc foreground={} bounds={:?}",
        wgpu_mask.iter().filter(|value| **value).count(),
        mask_bounds(&wgpu_mask, 64),
        blender_mask.iter().filter(|value| **value).count(),
        mask_bounds(&blender_mask, 64),
    );
    eprintln!(
        "wgpu depth min/max={:?}; BlenderProc depth min/max={:?}",
        wgpu_depth
            .iter()
            .copied()
            .min_by(f32::total_cmp)
            .zip(wgpu_depth.iter().copied().max_by(f32::total_cmp)),
        blender_depth
            .iter()
            .copied()
            .min_by(f32::total_cmp)
            .zip(blender_depth.iter().copied().max_by(f32::total_cmp)),
    );
    let iou = one_pixel_tolerant_iou(&wgpu_mask, &blender_mask, 64, 64);
    assert!(
        iou >= 0.97,
        "one-pixel-tolerant visible-mask IoU {iou} is below 0.97"
    );

    let mut errors: Vec<f32> = wgpu_depth
        .iter()
        .zip(&blender_depth)
        .zip(wgpu_eroded.iter().zip(&blender_eroded))
        .filter_map(|((left, right), (left_mask, right_mask))| {
            (*left_mask && *right_mask).then_some((left - right).abs())
        })
        .collect();
    assert!(!errors.is_empty(), "no common eroded foreground remained");
    let median = nearest_rank(&mut errors.clone(), 1, 2);
    let p99 = nearest_rank(&mut errors, 99, 100);
    eprintln!("one-pixel-tolerant IoU={iou}; depth median={median} m; p99={p99} m");
    assert!(
        median <= 0.001,
        "median depth difference {median} m exceeds 1 mm"
    );
    assert!(p99 <= 0.005, "p99 depth difference {p99} m exceeds 5 mm");
}

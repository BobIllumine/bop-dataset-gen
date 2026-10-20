//! Export of a canonical dataset to the BOP 6D-pose format.
//!
//! The layout follows the BOP dataset format
//! (<https://github.com/thodan/bop_toolkit/blob/master/docs/bop_datasets_format.md>):
//!
//! ```text
//! models/models_info.json, models/obj_NNNNNN.ply   (millimetres)
//! camera.json                                       (only when every camera is identical)
//! train_pbr/SSSSSS/scene_camera.json, scene_gt.json
//! train_pbr/SSSSSS/scene_gt_bop.json                (instance and bin of each scene_gt entry)
//! train_pbr/SSSSSS/rgb/IIIIII.png, depth/IIIIII.png, mask_visib/IIIIII_GGGGGG.png
//! bop_objects.json, bop_export.json                 (provenance, not part of BOP)
//! ```
//!
//! One canonical scene becomes one BOP scene and each camera becomes one image
//! whose `im_id` is the camera ID. Object IDs are assigned 1, 2, … in asset-ID
//! order. Ground-truth entries and visible masks follow instance-ID order.
//! `scene_gt_bop.json` lists, in the same order as `scene_gt.json`, each
//! entry's canonical `instance_id` and the `bin_id` it was dropped into.
//! Amodal masks (`mask/`) and `scene_gt_info.json` are not written.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
};

use bop_core::{AssetId, CameraSnapshot, RigidTransform, SceneSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    CanonicalModel, CanonicalModelError, DatasetValidationError, read_canonical_models,
    validation::{read_depth, read_json},
};

/// BOP split directory written by the exporter.
pub const BOP_SPLIT: &str = "train_pbr";

/// Largest scene ID representable by BOP's six-digit scene directories.
const MAX_BOP_SCENE_ID: u64 = 999_999;

/// Choices that shape a BOP export.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BopExportOptions {
    depth_scale_mm: f64,
}

impl BopExportOptions {
    /// Uses `depth_scale_mm` millimetres per 16-bit depth unit.
    ///
    /// # Errors
    ///
    /// Returns [`BopExportError::InvalidDepthScale`] unless the scale is finite and positive.
    pub fn try_new(depth_scale_mm: f64) -> Result<Self, BopExportError> {
        if depth_scale_mm.is_finite() && depth_scale_mm > 0.0 {
            Ok(Self { depth_scale_mm })
        } else {
            Err(BopExportError::InvalidDepthScale)
        }
    }

    /// Returns millimetres per 16-bit depth unit.
    #[must_use]
    pub const fn depth_scale_mm(self) -> f64 {
        self.depth_scale_mm
    }
}

impl Default for BopExportOptions {
    /// 0.1 mm per unit, as in the BOP PBR training sets; the range is 6.5535 m.
    fn default() -> Self {
        Self {
            depth_scale_mm: 0.1,
        }
    }
}

/// Counts from a finished export.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct BopReport {
    /// Exported object models.
    pub object_count: u64,
    /// Exported BOP scenes.
    pub scene_count: u64,
    /// Exported images (scene × camera).
    pub image_count: u64,
    /// Exported ground-truth object instances, summed over images.
    pub ground_truth_count: u64,
}

/// Exports a validated canonical dataset to `output_root` in BOP format.
///
/// The dataset is validated first. Output is written to a sibling staging
/// directory, validated with [`validate_bop`], and renamed into place, so
/// `output_root` either does not exist or holds a complete export.
///
/// # Errors
///
/// Returns [`BopExportError`] when the dataset is invalid or incomplete,
/// `output_root` already exists, depth exceeds the 16-bit range at the chosen
/// scale, a pose cannot be represented, or I/O fails.
pub fn export_bop(
    dataset_root: &Path,
    output_root: &Path,
    options: BopExportOptions,
) -> Result<BopReport, BopExportError> {
    crate::validate_dataset(dataset_root)?;
    if output_root.exists() {
        return Err(BopExportError::OutputExists {
            path: output_root.to_owned(),
        });
    }
    let models = read_canonical_models(dataset_root)?;
    let name = output_root
        .file_name()
        .ok_or_else(|| BopExportError::OutputExists {
            path: output_root.to_owned(),
        })?
        .to_string_lossy();
    let parent = output_root
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(io(parent))?;
    let staging = parent.join(format!(".{name}.bop-{}.tmp", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(io(&staging))?;
    }
    let result = write_export(dataset_root, &staging, &models, options).and_then(|report| {
        let validated = validate_bop(&staging)?;
        if validated != report {
            return Err(BopExportError::Validation(BopValidationError::Invalid {
                path: staging.clone(),
                reason: "validated counts differ from exported counts".to_owned(),
            }));
        }
        fs::rename(&staging, output_root).map_err(io(output_root))?;
        Ok(report)
    });
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn write_export(
    dataset_root: &Path,
    root: &Path,
    models: &[CanonicalModel],
    options: BopExportOptions,
) -> Result<BopReport, BopExportError> {
    let object_ids = models
        .iter()
        .zip(1_u32..)
        .map(|(model, obj_id)| (model.asset_id().clone(), obj_id))
        .collect::<BTreeMap<AssetId, u32>>();
    write_models(root, models, &object_ids)?;
    let mut report = BopReport {
        object_count: models.len() as u64,
        scene_count: 0,
        image_count: 0,
        ground_truth_count: 0,
    };
    let mut cameras = BTreeSet::new();
    for scene_dir in sorted_children(&dataset_root.join("scenes"))? {
        write_scene(
            &scene_dir,
            root,
            &object_ids,
            options,
            &mut cameras,
            &mut report,
        )?;
        report.scene_count += 1;
    }
    if cameras.len() == 1 {
        let first = scene_camera_of_first_image(root)?;
        write_json(&root.join("camera.json"), &first)?;
    }
    let manifest: Value = read_json(&dataset_root.join("run.json"))?;
    write_json(
        &root.join("bop_export.json"),
        &json!({
            "schema_version": 1,
            "split": BOP_SPLIT,
            "depth_scale": options.depth_scale_mm,
            "source_run": manifest,
            "omitted": ["mask", "scene_gt_info.json"],
        }),
    )?;
    Ok(report)
}

fn sorted_children(directory: &Path) -> Result<Vec<PathBuf>, BopExportError> {
    let mut children = fs::read_dir(directory)
        .map_err(io(directory))?
        .map(|entry| entry.map(|entry| entry.path()).map_err(io(directory)))
        .collect::<Result<Vec<_>, _>>()?;
    children.sort();
    Ok(children)
}

/// Writes one canonical scene as one BOP scene with one image per camera.
fn write_scene(
    scene_dir: &Path,
    root: &Path,
    object_ids: &BTreeMap<AssetId, u32>,
    options: BopExportOptions,
    cameras: &mut BTreeSet<String>,
    report: &mut BopReport,
) -> Result<(), BopExportError> {
    let scene: SceneSnapshot = read_json(&scene_dir.join("scene.json"))?;
    let scene_id = scene.scene_id().get();
    if scene_id > MAX_BOP_SCENE_ID {
        return Err(BopExportError::SceneIdTooLarge { scene_id });
    }
    let out = root.join(BOP_SPLIT).join(format!("{scene_id:06}"));
    let mut instances = scene.instances().to_vec();
    instances.sort_by_key(|instance| instance.instance_id().get());
    let obj_ids = instances
        .iter()
        .map(|instance| {
            object_ids.get(instance.asset_id()).copied().ok_or_else(|| {
                BopExportError::UnknownAsset {
                    scene_id,
                    asset_id: instance.asset_id().as_str().to_owned(),
                }
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut scene_camera = BTreeMap::new();
    let mut scene_gt = BTreeMap::new();
    let mut scene_gt_bop = BTreeMap::new();
    for frame_dir in sorted_children(&scene_dir.join("frames"))? {
        let camera: CameraSnapshot = read_json(&frame_dir.join("frame.json"))?;
        let im_id = camera.camera_id().get();
        let size = camera.image_size();
        let camera_from_world = camera.camera_from_world().camera_from_world();
        let (rotation_w2c, translation_w2c) = pose_mm(camera_from_world);
        scene_camera.insert(
            im_id.to_string(),
            json!({
                "cam_K": camera.k().k().map(shortest_decimal),
                "depth_scale": options.depth_scale_mm,
                "cam_R_w2c": rotation_w2c,
                "cam_t_w2c": translation_w2c,
            }),
        );
        cameras.insert(
            json!({"k": camera.k().k(), "size": [size.width(), size.height()]}).to_string(),
        );
        let mut ground_truth = Vec::with_capacity(instances.len());
        for (instance, obj_id) in instances.iter().zip(&obj_ids) {
            let (rotation, translation) = compose_mm(
                pose_mm(camera_from_world),
                pose_mm(instance.world_from_model()),
            );
            ground_truth.push(json!({
                "cam_R_m2c": rotation,
                "cam_t_m2c": translation,
                "obj_id": obj_id,
            }));
        }
        report.ground_truth_count += ground_truth.len() as u64;
        scene_gt.insert(im_id.to_string(), Value::Array(ground_truth));
        scene_gt_bop.insert(
            im_id.to_string(),
            instances
                .iter()
                .map(|instance| {
                    json!({
                        "instance_id": instance.instance_id().get(),
                        "bin_id": instance.bin_id(),
                    })
                })
                .collect::<Vec<_>>(),
        );
        write_images(&frame_dir, &out, im_id, size, &instances, options)?;
        report.image_count += 1;
    }
    write_json(&out.join("scene_camera.json"), &scene_camera)?;
    write_json(&out.join("scene_gt.json"), &scene_gt)?;
    write_json(&out.join("scene_gt_bop.json"), &scene_gt_bop)
}

/// Writes the RGB, 16-bit depth, and visible-mask images of one camera.
fn write_images(
    frame_dir: &Path,
    out: &Path,
    im_id: u16,
    size: bop_core::ImageSize,
    instances: &[bop_core::InstanceSnapshot],
    options: BopExportOptions,
) -> Result<(), BopExportError> {
    let stem = format!("{im_id:06}");
    let rgb = frame_dir.join("rgb.png");
    if rgb.exists() {
        let target = out.join("rgb").join(format!("{stem}.png"));
        create_parent(&target)?;
        fs::copy(&rgb, &target).map_err(io(&target))?;
    }
    let depth = frame_dir.join("depth.exr");
    if depth.exists() {
        let metres = read_depth(&depth, size.width(), size.height())?;
        let units = quantize_depth(&metres, options.depth_scale_mm).ok_or_else(|| {
            BopExportError::DepthOverflow {
                path: depth.clone(),
                depth_scale_mm: options.depth_scale_mm,
            }
        })?;
        let bytes = units
            .iter()
            .flat_map(|unit| unit.to_be_bytes())
            .collect::<Vec<_>>();
        let target = out.join("depth").join(format!("{stem}.png"));
        write_png(
            &target,
            size.width(),
            size.height(),
            png::BitDepth::Sixteen,
            &bytes,
        )?;
    }
    let instance_png = frame_dir.join("instance.png");
    if instance_png.exists() {
        let ids = read_u16_png(&instance_png)?;
        for (gt_index, instance) in instances.iter().enumerate() {
            let wanted = instance.instance_id().get();
            let mask = ids
                .iter()
                .map(|id| if *id == wanted { 255 } else { 0 })
                .collect::<Vec<u8>>();
            let target = out
                .join("mask_visib")
                .join(format!("{stem}_{gt_index:06}.png"));
            write_png(
                &target,
                size.width(),
                size.height(),
                png::BitDepth::Eight,
                &mask,
            )?;
        }
    }
    Ok(())
}

/// Builds `camera.json` from the first image when every camera is identical.
fn scene_camera_of_first_image(root: &Path) -> Result<Value, BopExportError> {
    let split = root.join(BOP_SPLIT);
    let mut scenes = fs::read_dir(&split)
        .map_err(io(&split))?
        .map(|entry| entry.map(|entry| entry.path()).map_err(io(&split)))
        .collect::<Result<Vec<_>, _>>()?;
    scenes.sort();
    let first = scenes.first().ok_or_else(|| {
        BopExportError::Validation(BopValidationError::Invalid {
            path: split.clone(),
            reason: "no scenes".to_owned(),
        })
    })?;
    let cameras: BTreeMap<String, Value> = read_bop_json(&first.join("scene_camera.json"))?;
    let (im_id, camera) = cameras.iter().next().ok_or_else(|| {
        BopExportError::Validation(BopValidationError::Invalid {
            path: first.clone(),
            reason: "scene without images".to_owned(),
        })
    })?;
    let rgb = first
        .join("rgb")
        .join(format!("{:06}.png", im_id.parse::<u32>().unwrap_or(0)));
    let (width, height) = if rgb.exists() {
        png_size(&rgb)?
    } else {
        let depth = first
            .join("depth")
            .join(format!("{:06}.png", im_id.parse::<u32>().unwrap_or(0)));
        png_size(&depth)?
    };
    let k = &camera["cam_K"];
    Ok(json!({
        "cx": k[2], "cy": k[5], "fx": k[0], "fy": k[4],
        "width": width, "height": height,
        "depth_scale": camera["depth_scale"],
    }))
}

fn write_models(
    root: &Path,
    models: &[CanonicalModel],
    object_ids: &BTreeMap<AssetId, u32>,
) -> Result<(), BopExportError> {
    let directory = root.join("models");
    fs::create_dir_all(&directory).map_err(io(&directory))?;
    let mut info = BTreeMap::new();
    let mut mapping = Vec::new();
    for model in models {
        let obj_id = object_ids[model.asset_id()];
        let path = directory.join(format!("obj_{obj_id:06}.ply"));
        fs::write(&path, model.to_ply(1000.0)).map_err(io(&path))?;
        info.insert(obj_id.to_string(), model_info(model));
        mapping.push(json!({
            "obj_id": obj_id,
            "asset_id": model.asset_id().as_str(),
            "content_fingerprint": model.content_fingerprint(),
        }));
    }
    write_json(&directory.join("models_info.json"), &info)?;
    write_json(&root.join("bop_objects.json"), &mapping)
}

/// Returns the BOP `models_info.json` entry in millimetres.
///
/// The diameter is the largest distance between any two distinct vertices.
fn model_info(model: &CanonicalModel) -> Value {
    let points = model
        .positions()
        .iter()
        .map(|point| point.map(|value| f64::from(value) * 1000.0))
        .map(|point| point.map(f64::to_bits))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|point| point.map(f64::from_bits))
        .collect::<Vec<_>>();
    let mut minimum = [f64::INFINITY; 3];
    let mut maximum = [f64::NEG_INFINITY; 3];
    for point in &points {
        for axis in 0..3 {
            minimum[axis] = minimum[axis].min(point[axis]);
            maximum[axis] = maximum[axis].max(point[axis]);
        }
    }
    let mut diameter_squared = 0.0_f64;
    for (index, a) in points.iter().enumerate() {
        for b in &points[index + 1..] {
            let distance = (0..3).map(|axis| (a[axis] - b[axis]).powi(2)).sum::<f64>();
            diameter_squared = diameter_squared.max(distance);
        }
    }
    json!({
        "diameter": diameter_squared.sqrt(),
        "min_x": minimum[0], "min_y": minimum[1], "min_z": minimum[2],
        "size_x": maximum[0] - minimum[0],
        "size_y": maximum[1] - minimum[1],
        "size_z": maximum[2] - minimum[2],
    })
}

/// Widens an f32 to the f64 nearest its shortest round-trip decimal, so
/// `10.392305_f32` is written as `10.392305` rather than `10.392305374145508`.
fn shortest_decimal(value: f32) -> f64 {
    value
        .to_string()
        .parse()
        .unwrap_or_else(|_| f64::from(value))
}

/// Composes two row-major millimetre poses, `outer ∘ inner`, in f64.
fn compose_mm(outer: ([f64; 9], [f64; 3]), inner: ([f64; 9], [f64; 3])) -> ([f64; 9], [f64; 3]) {
    let (a, at) = outer;
    let (b, bt) = inner;
    let mut rotation = [0.0; 9];
    let mut translation = at;
    for row in 0..3 {
        for column in 0..3 {
            rotation[row * 3 + column] = (0..3).map(|k| a[row * 3 + k] * b[k * 3 + column]).sum();
        }
        translation[row] += (0..3).map(|k| a[row * 3 + k] * bt[k]).sum::<f64>();
    }
    (rotation, translation)
}

/// Returns a row-major rotation and a translation in millimetres.
fn pose_mm(transform: RigidTransform) -> ([f64; 9], [f64; 3]) {
    let quaternion = transform.rotation_xyzw().map(f64::from);
    let norm = quaternion
        .iter()
        .map(|value| value * value)
        .sum::<f64>()
        .sqrt();
    let [x, y, z, w] = quaternion.map(|value| value / norm);
    let rotation = [
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y - z * w),
        2.0 * (x * z + y * w),
        2.0 * (x * y + z * w),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z - x * w),
        2.0 * (x * z - y * w),
        2.0 * (y * z + x * w),
        1.0 - 2.0 * (x * x + y * y),
    ];
    let translation = transform
        .translation()
        .map(|value| f64::from(value.get()) * 1000.0);
    (rotation, translation)
}

/// Converts metric depth to 16-bit units, or `None` if any pixel overflows.
fn quantize_depth(metres: &[f32], depth_scale_mm: f64) -> Option<Vec<u16>> {
    metres
        .iter()
        .map(|depth| {
            let units = (f64::from(*depth) * 1000.0 / depth_scale_mm).round();
            // Depth is validated nonnegative, so the range check makes the cast exact.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            (0.0..=f64::from(u16::MAX))
                .contains(&units)
                .then_some(units as u16)
        })
        .collect()
}

fn create_parent(path: &Path) -> Result<(), BopExportError> {
    let parent = path.parent().expect("export paths have a parent");
    fs::create_dir_all(parent).map_err(io(parent))
}

fn write_png(
    path: &Path,
    width: u32,
    height: u32,
    depth: png::BitDepth,
    bytes: &[u8],
) -> Result<(), BopExportError> {
    create_parent(path)?;
    let file = File::create(path).map_err(io(path))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(depth);
    let png_error = |error: png::EncodingError| BopExportError::Png {
        path: path.to_owned(),
        message: error.to_string(),
    };
    let mut writer = encoder.write_header().map_err(png_error)?;
    writer.write_image_data(bytes).map_err(png_error)?;
    writer.finish().map_err(png_error)
}

fn read_png(path: &Path) -> Result<(png::OutputInfo, Vec<u8>), BopValidationError> {
    let file = File::open(path).map_err(|source| BopValidationError::Io {
        path: path.to_owned(),
        source,
    })?;
    let png_error = |error: png::DecodingError| BopValidationError::Invalid {
        path: path.to_owned(),
        reason: error.to_string(),
    };
    let mut reader = png::Decoder::new(BufReader::new(file))
        .read_info()
        .map_err(png_error)?;
    let mut bytes = vec![0; reader.output_buffer_size().unwrap_or(0)];
    let info = reader.next_frame(&mut bytes).map_err(png_error)?;
    bytes.truncate(info.buffer_size());
    Ok((info, bytes))
}

fn png_size(path: &Path) -> Result<(u32, u32), BopValidationError> {
    let file = File::open(path).map_err(|source| BopValidationError::Io {
        path: path.to_owned(),
        source,
    })?;
    let reader = png::Decoder::new(BufReader::new(file))
        .read_info()
        .map_err(|error| BopValidationError::Invalid {
            path: path.to_owned(),
            reason: error.to_string(),
        })?;
    let info = reader.info();
    Ok((info.width, info.height))
}

fn read_u16_png(path: &Path) -> Result<Vec<u16>, BopValidationError> {
    let (_, bytes) = read_png(path)?;
    Ok(bytes
        .chunks_exact(2)
        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
        .collect())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), BopExportError> {
    create_parent(path)?;
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|source| BopExportError::Json {
        path: path.to_owned(),
        source,
    })?;
    bytes.push(b'\n');
    fs::write(path, bytes).map_err(io(path))
}

fn read_bop_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, BopValidationError> {
    let bytes = fs::read(path).map_err(|source| BopValidationError::Io {
        path: path.to_owned(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|error| BopValidationError::Invalid {
        path: path.to_owned(),
        reason: error.to_string(),
    })
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> BopExportError + '_ {
    move |source| BopExportError::Io {
        path: path.to_owned(),
        source,
    }
}

#[derive(Deserialize)]
struct CameraEntry {
    #[serde(rename = "cam_K")]
    cam_k: [f64; 9],
    depth_scale: f64,
    #[serde(rename = "cam_R_w2c")]
    cam_r_w2c: [f64; 9],
    #[serde(rename = "cam_t_w2c")]
    cam_t_w2c: [f64; 3],
}

#[derive(Deserialize)]
struct GroundTruthEntry {
    #[serde(rename = "cam_R_m2c")]
    cam_r_m2c: [f64; 9],
    #[serde(rename = "cam_t_m2c")]
    cam_t_m2c: [f64; 3],
    obj_id: u32,
}

/// Checks the structure and internal consistency of a BOP export.
///
/// Every model listed in `models_info.json` must exist. Every scene must have
/// matching `scene_camera.json` and `scene_gt.json` keys, finite
/// intrinsics, a positive depth scale, orthonormal right-handed rotations,
/// known object IDs, and image, depth, and mask files of one size, with one
/// visible mask per ground-truth entry.
///
/// # Errors
///
/// Returns [`BopValidationError`] naming the first offending file.
pub fn validate_bop(root: &Path) -> Result<BopReport, BopValidationError> {
    let info_path = root.join("models").join("models_info.json");
    let info: BTreeMap<String, Value> = read_bop_json(&info_path)?;
    let mut object_ids = BTreeSet::new();
    for (key, entry) in &info {
        let obj_id = key
            .parse::<u32>()
            .map_err(|_| invalid(&info_path, "object ID is not an integer"))?;
        let diameter = entry["diameter"].as_f64().unwrap_or(-1.0);
        if !(diameter.is_finite() && diameter > 0.0) {
            return Err(invalid(&info_path, "diameter must be positive"));
        }
        let model = root.join("models").join(format!("obj_{obj_id:06}.ply"));
        let header = fs::read(&model).map_err(|source| BopValidationError::Io {
            path: model.clone(),
            source,
        })?;
        if !header.starts_with(b"ply\n") {
            return Err(invalid(&model, "not a PLY file"));
        }
        object_ids.insert(obj_id);
    }
    if object_ids.is_empty() {
        return Err(invalid(&info_path, "no models"));
    }

    let split = root.join(BOP_SPLIT);
    let mut report = BopReport {
        object_count: object_ids.len() as u64,
        scene_count: 0,
        image_count: 0,
        ground_truth_count: 0,
    };
    let entries = fs::read_dir(&split).map_err(|source| BopValidationError::Io {
        path: split.clone(),
        source,
    })?;
    for entry in entries {
        let scene = entry
            .map_err(|source| BopValidationError::Io {
                path: split.clone(),
                source,
            })?
            .path();
        validate_scene(&scene, &object_ids, &mut report)?;
        report.scene_count += 1;
    }
    if report.scene_count == 0 {
        return Err(invalid(&split, "no scenes"));
    }
    Ok(report)
}

fn invalid(path: &Path, reason: &str) -> BopValidationError {
    BopValidationError::Invalid {
        path: path.to_owned(),
        reason: reason.to_owned(),
    }
}

fn validate_scene(
    scene: &Path,
    object_ids: &BTreeSet<u32>,
    report: &mut BopReport,
) -> Result<(), BopValidationError> {
    let name = scene
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if name.len() != 6 || !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid(scene, "scene directory is not six digits"));
    }
    let cameras: BTreeMap<String, CameraEntry> = read_bop_json(&scene.join("scene_camera.json"))?;
    let ground_truth: BTreeMap<String, Vec<GroundTruthEntry>> =
        read_bop_json(&scene.join("scene_gt.json"))?;
    if cameras.keys().ne(ground_truth.keys()) || cameras.is_empty() {
        return Err(invalid(scene, "scene_camera and scene_gt image IDs differ"));
    }
    for (key, camera) in &cameras {
        let im_id = key
            .parse::<u32>()
            .map_err(|_| invalid(scene, "image ID is not an integer"))?;
        let stem = format!("{im_id:06}");
        let valid_camera = camera.cam_k.iter().all(|value| value.is_finite())
            && camera.depth_scale.is_finite()
            && camera.depth_scale > 0.0
            && is_rotation(&camera.cam_r_w2c)
            && camera.cam_t_w2c.iter().all(|value| value.is_finite());
        if !valid_camera {
            return Err(invalid(
                &scene.join("scene_camera.json"),
                "invalid camera entry",
            ));
        }
        let mut size = None;
        let mut check_size = |path: &Path| -> Result<(), BopValidationError> {
            let actual = png_size(path)?;
            if *size.get_or_insert(actual) != actual {
                return Err(invalid(path, "image size differs from the other channels"));
            }
            Ok(())
        };
        for channel in ["rgb", "depth"] {
            let path = scene.join(channel).join(format!("{stem}.png"));
            if path.exists() {
                check_size(&path)?;
            }
        }
        let entries = &ground_truth[key];
        let masks = scene.join("mask_visib");
        for (gt_index, entry) in entries.iter().enumerate() {
            let valid_entry = object_ids.contains(&entry.obj_id)
                && is_rotation(&entry.cam_r_m2c)
                && entry.cam_t_m2c.iter().all(|value| value.is_finite());
            if !valid_entry {
                return Err(invalid(
                    &scene.join("scene_gt.json"),
                    "invalid ground-truth entry",
                ));
            }
            if masks.exists() {
                check_size(&masks.join(format!("{stem}_{gt_index:06}.png")))?;
            }
        }
        if size.is_none() {
            return Err(invalid(scene, "image has no channels"));
        }
        report.image_count += 1;
        report.ground_truth_count += entries.len() as u64;
    }
    Ok(())
}

/// Reports whether a row-major 3×3 matrix is a proper rotation.
fn is_rotation(matrix: &[f64; 9]) -> bool {
    const TOLERANCE: f64 = 1e-5;
    if !matrix.iter().all(|value| value.is_finite()) {
        return false;
    }
    let row = |index: usize| {
        [
            matrix[index * 3],
            matrix[index * 3 + 1],
            matrix[index * 3 + 2],
        ]
    };
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    for i in 0..3 {
        for j in 0..3 {
            let expected = if i == j { 1.0 } else { 0.0 };
            if (dot(row(i), row(j)) - expected).abs() > TOLERANCE {
                return false;
            }
        }
    }
    let [a, b, c] = [row(0), row(1), row(2)];
    let determinant = a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
        + a[2] * (b[0] * c[1] - b[1] * c[0]);
    (determinant - 1.0).abs() <= TOLERANCE
}

/// A canonical dataset could not be exported to BOP.
#[derive(Debug, Error)]
pub enum BopExportError {
    /// The source dataset is invalid or incomplete.
    #[error(transparent)]
    Dataset(#[from] DatasetValidationError),
    /// The source dataset's models cannot be read.
    #[error(transparent)]
    Models(#[from] CanonicalModelError),
    /// The output path already exists or has no file name.
    #[error("export output {path} already exists")]
    OutputExists {
        /// Rejected output path.
        path: PathBuf,
    },
    /// The depth scale is not finite and positive.
    #[error("depth scale must be finite and positive")]
    InvalidDepthScale,
    /// A depth pixel does not fit 16 bits at the chosen scale.
    #[error("depth in {path} exceeds 65535 units at {depth_scale_mm} mm per unit")]
    DepthOverflow {
        /// Source depth file.
        path: PathBuf,
        /// Scale in use.
        depth_scale_mm: f64,
    },
    /// A scene ID does not fit BOP's six-digit scene directories.
    #[error("scene {scene_id} does not fit a six-digit BOP scene directory")]
    SceneIdTooLarge {
        /// Offending scene.
        scene_id: u64,
    },
    /// A scene references an asset with no stored model.
    #[error("scene {scene_id} references asset {asset_id} with no stored model")]
    UnknownAsset {
        /// Offending scene.
        scene_id: u64,
        /// Missing asset.
        asset_id: String,
    },
    /// Filesystem access failed.
    #[error("BOP export I/O failed at {path}: {source}")]
    Io {
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// JSON serialization failed.
    #[error("cannot serialize {path}: {source}")]
    Json {
        /// Target file.
        path: PathBuf,
        /// Serialization failure.
        #[source]
        source: serde_json::Error,
    },
    /// PNG encoding failed.
    #[error("cannot encode {path}: {message}")]
    Png {
        /// Target file.
        path: PathBuf,
        /// Encoder message.
        message: String,
    },
    /// The written export failed validation.
    #[error(transparent)]
    Validation(#[from] BopValidationError),
}

/// A BOP export is malformed.
#[derive(Debug, Error)]
pub enum BopValidationError {
    /// Filesystem access failed.
    #[error("BOP I/O failed at {path}: {source}")]
    Io {
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A file is missing, malformed, or inconsistent.
    #[error("invalid BOP file {path}: {reason}")]
    Invalid {
        /// Offending file or directory.
        path: PathBuf,
        /// What is wrong.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_quantization_rounds_and_rejects_overflow() {
        assert_eq!(
            quantize_depth(&[0.0, 0.00004, 0.00006, 1.0], 0.1),
            Some(vec![0, 0, 1, 10_000])
        );
        assert_eq!(quantize_depth(&[6.5535], 0.1), Some(vec![65_535]));
        assert_eq!(quantize_depth(&[6.5536], 0.1), None);
    }

    #[test]
    fn quaternion_rotations_are_proper() {
        let quarter_turn_z =
            RigidTransform::try_new([0.1, 0.0, 0.0], [0.0, 0.0, 0.707_106_77, 0.707_106_77])
                .expect("transform");
        let (rotation, translation) = pose_mm(quarter_turn_z);
        assert!(is_rotation(&rotation));
        assert!((rotation[1] + 1.0).abs() < 1e-6 && (rotation[3] - 1.0).abs() < 1e-6);
        assert!((translation[0] - 100.0).abs() < 1e-3);
        assert!(!is_rotation(&[
            1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, -1.0
        ]));
    }

    #[test]
    fn options_reject_nonpositive_scales() {
        assert!(BopExportOptions::try_new(0.0).is_err());
        assert!(BopExportOptions::try_new(f64::NAN).is_err());
        assert!((BopExportOptions::default().depth_scale_mm() - 0.1).abs() < f64::EPSILON);
    }
}

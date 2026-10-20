use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufReader, Write},
    path::{Path, PathBuf},
};

use bop_core::{CameraSnapshot, RenderSceneRequest, SceneId, SceneSnapshot};
use exr::prelude::{ReadChannels as _, ReadLayers as _, ReadSpecificChannel as _, read};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::SceneStagingArea;

/// Summary of a successful, read-only validation of a committed dataset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DatasetValidationReport {
    scene_count: u64,
    file_count: u64,
}

impl DatasetValidationReport {
    /// Returns the number of validated committed scenes.
    #[must_use]
    pub const fn scene_count(self) -> u64 {
        self.scene_count
    }

    /// Returns the number of content files covered by stored checksums.
    #[must_use]
    pub const fn file_count(self) -> u64 {
        self.file_count
    }
}

/// Validates the immutable identity and checksums of a committed dataset.
///
/// This operation is read-only. It requires every scene in the manifest domain,
/// rejects noncanonical scene directories, and verifies every checksum entry.
///
/// # Errors
///
/// Returns a path-specific error for malformed manifests, missing scenes,
/// unsafe checksum paths, untracked files, or content whose BLAKE3 digest changed.
pub fn validate_dataset(root: &Path) -> Result<DatasetValidationReport, DatasetValidationError> {
    let manifest_path = root.join("run.json");
    let manifest: crate::DatasetManifest = read_json(&manifest_path)?;
    let config_path = root.join("resolved-config.toml");
    let config = fs::read(&config_path).map_err(|source| io_error(&config_path, source))?;
    if crate::resume::fingerprint_config(&config) != manifest.resolved_config_fingerprint() {
        return Err(DatasetValidationError::ResolvedConfigFingerprint);
    }
    let environment_path = root.join("environment.json");
    let _: crate::RunEnvironment = read_json(&environment_path)?;

    let scenes_root = root.join("scenes");
    let entries = fs::read_dir(&scenes_root).map_err(|source| io_error(&scenes_root, source))?;
    let mut seen = BTreeSet::new();
    let mut file_count = 0_u64;
    for entry in entries {
        let entry = entry.map_err(|source| io_error(&scenes_root, source))?;
        let path = entry.path();
        if !entry
            .file_type()
            .map_err(|source| io_error(&path, source))?
            .is_dir()
        {
            return Err(DatasetValidationError::InvalidScenePath { path });
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(DatasetValidationError::InvalidScenePath { path });
        };
        let Ok(scene_id) = name.parse::<u64>() else {
            return Err(DatasetValidationError::InvalidScenePath { path });
        };
        if name != format!("{scene_id:020}") || scene_id >= manifest.total_scene_count() {
            return Err(DatasetValidationError::InvalidScenePath { path });
        }
        if !seen.insert(scene_id) {
            return Err(DatasetValidationError::InvalidScenePath { path });
        }
        file_count = file_count
            .checked_add(validate_committed_scene(&path, scene_id)?)
            .ok_or(DatasetValidationError::CountOverflow)?;
    }
    if seen.len() as u64 != manifest.total_scene_count() {
        return Err(DatasetValidationError::IncompleteDataset {
            expected: manifest.total_scene_count(),
            actual: seen.len() as u64,
        });
    }
    Ok(DatasetValidationReport {
        scene_count: manifest.total_scene_count(),
        file_count,
    })
}

fn validate_committed_scene(
    root: &Path,
    expected_scene_id: u64,
) -> Result<u64, DatasetValidationError> {
    let checksum_path = root.join("checksums.json");
    let checksums: StoredChecksumManifest = read_json(&checksum_path)?;
    if checksums.schema_version != 1 || checksums.scene_id != expected_scene_id {
        return Err(DatasetValidationError::ChecksumManifestIdentity {
            path: checksum_path,
        });
    }
    let actual_files = collect_content_files(root)?;
    let declared = checksums.files.keys().cloned().collect::<BTreeSet<_>>();
    if actual_files != declared {
        return Err(DatasetValidationError::FileSetMismatch {
            path: root.to_owned(),
        });
    }
    for (relative, expected) in &checksums.files {
        let relative_path = Path::new(relative);
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err(DatasetValidationError::UnsafeChecksumPath {
                path: relative_path.to_owned(),
            });
        }
        let path = root.join(relative_path);
        let bytes = fs::read(&path).map_err(|source| io_error(&path, source))?;
        let actual = blake3::hash(&bytes).to_hex().to_string();
        if &actual != expected {
            return Err(DatasetValidationError::ChecksumMismatch { path });
        }
    }
    validate_scene_content(root, expected_scene_id, &declared)?;
    u64::try_from(checksums.files.len()).map_err(|_| DatasetValidationError::CountOverflow)
}

fn validate_scene_content(
    root: &Path,
    expected_scene_id: u64,
    declared: &BTreeSet<String>,
) -> Result<(), DatasetValidationError> {
    let scene_path = root.join("scene.json");
    let scene: SceneSnapshot = read_json(&scene_path)?;
    if scene.scene_id().get() != expected_scene_id {
        return Err(DatasetValidationError::SceneMetadataIdentity { path: scene_path });
    }
    let legal_ids = scene
        .instances()
        .iter()
        .map(|instance| instance.instance_id().get())
        .chain(std::iter::once(0))
        .collect::<BTreeSet<_>>();
    if declared.contains("materials.json") {
        let path = root.join("materials.json");
        let materials: StoredSceneMaterials = read_json(&path)?;
        if materials.schema_version != 1
            || materials
                .instances
                .iter()
                .any(|material| !legal_ids.contains(&material.instance_id().get()))
        {
            return Err(DatasetValidationError::MaterialMetadata { path });
        }
    }
    let frames = root.join("frames");
    let entries = fs::read_dir(&frames).map_err(|source| io_error(&frames, source))?;
    let mut camera_count = 0_u64;
    let mut channel_shape = None;
    for entry in entries {
        let entry = entry.map_err(|source| io_error(&frames, source))?;
        let directory = entry.path();
        if !entry
            .file_type()
            .map_err(|source| io_error(&directory, source))?
            .is_dir()
        {
            return Err(DatasetValidationError::InvalidFramePath { path: directory });
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(DatasetValidationError::InvalidFramePath { path: directory });
        };
        let Ok(camera_id) = name.parse::<u16>() else {
            return Err(DatasetValidationError::InvalidFramePath { path: directory });
        };
        if name != format!("{camera_id:05}") {
            return Err(DatasetValidationError::InvalidFramePath { path: directory });
        }
        let metadata_path = directory.join("frame.json");
        let camera: CameraSnapshot = read_json(&metadata_path)?;
        if camera.camera_id().get() != camera_id {
            return Err(DatasetValidationError::CameraMetadataIdentity {
                path: metadata_path,
            });
        }
        let prefix = format!("frames/{name}/");
        let shape = ["rgb.png", "depth.exr", "instance.png"]
            .map(|artifact| declared.contains(&format!("{prefix}{artifact}")));
        if !shape.iter().any(|present| *present)
            || channel_shape.is_some_and(|prior| prior != shape)
        {
            return Err(DatasetValidationError::InconsistentChannels { path: directory });
        }
        channel_shape = Some(shape);
        let size = camera.image_size();
        if shape[0] {
            validate_png(
                &directory.join("rgb.png"),
                size.width(),
                size.height(),
                png::ColorType::Rgb,
                png::BitDepth::Eight,
                None,
            )?;
        }
        if shape[1] {
            validate_depth(&directory.join("depth.exr"), size.width(), size.height())?;
        }
        if shape[2] {
            validate_png(
                &directory.join("instance.png"),
                size.width(),
                size.height(),
                png::ColorType::Grayscale,
                png::BitDepth::Sixteen,
                Some(&legal_ids),
            )?;
        }
        camera_count = camera_count
            .checked_add(1)
            .ok_or(DatasetValidationError::CountOverflow)?;
    }
    if camera_count == 0 {
        return Err(DatasetValidationError::EmptyFrames { path: frames });
    }
    Ok(())
}

fn collect_content_files(root: &Path) -> Result<BTreeSet<String>, DatasetValidationError> {
    fn visit(
        base: &Path,
        directory: &Path,
        files: &mut BTreeSet<String>,
    ) -> Result<(), DatasetValidationError> {
        for entry in fs::read_dir(directory).map_err(|source| io_error(directory, source))? {
            let entry = entry.map_err(|source| io_error(directory, source))?;
            let path = entry.path();
            let kind = entry
                .file_type()
                .map_err(|source| io_error(&path, source))?;
            if kind.is_dir() {
                visit(base, &path, files)?;
            } else if kind.is_file()
                && path.file_name().and_then(|name| name.to_str()) != Some("checksums.json")
            {
                let relative = path
                    .strip_prefix(base)
                    .expect("walked dataset child")
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(relative);
            } else if !kind.is_file() {
                return Err(DatasetValidationError::InvalidScenePath { path });
            }
        }
        Ok(())
    }
    let mut files = BTreeSet::new();
    visit(root, root, &mut files)?;
    Ok(files)
}

pub(crate) fn read_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<T, DatasetValidationError> {
    let bytes = fs::read(path).map_err(|source| io_error(path, source))?;
    serde_json::from_slice(&bytes).map_err(|source| DatasetValidationError::Json {
        path: path.to_owned(),
        source,
    })
}

/// `materials.json`: the surface each instance was rendered with.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSceneMaterials {
    schema_version: u16,
    instances: Vec<bop_core::InstanceMaterial>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredChecksumManifest {
    schema_version: u16,
    scene_id: u64,
    files: BTreeMap<String, String>,
}

/// Validates every requested staged frame and writes deterministic BLAKE3 checksums.
///
/// # Errors
///
/// Returns a path-specific error for malformed encodings, wrong dimensions or
/// channel formats, invalid metric depth, illegal instance IDs, or I/O failure.
pub fn validate_and_write_checksums(
    staging: &SceneStagingArea,
    request: &RenderSceneRequest,
) -> Result<PathBuf, DatasetValidationError> {
    let legal_ids: BTreeSet<u16> = request
        .scene()
        .instances()
        .iter()
        .map(|instance| instance.instance_id().get())
        .chain(std::iter::once(0))
        .collect();
    for camera in request.cameras() {
        let size = camera.image_size();
        if request.required_channels().rgb() {
            validate_png(
                &staging.rgb_path(camera.camera_id()),
                size.width(),
                size.height(),
                png::ColorType::Rgb,
                png::BitDepth::Eight,
                None,
            )?;
        }
        if request.required_channels().instance() {
            validate_png(
                &staging.instance_path(camera.camera_id()),
                size.width(),
                size.height(),
                png::ColorType::Grayscale,
                png::BitDepth::Sixteen,
                Some(&legal_ids),
            )?;
        }
        if request.required_channels().depth() {
            validate_depth(
                &staging.depth_path(camera.camera_id()),
                size.width(),
                size.height(),
            )?;
        }
    }
    write_checksums(staging, request.scene().scene_id(), request)
}

/// Failure while validating renderer output before publication.
#[derive(Debug, Error)]
pub enum DatasetValidationError {
    /// A JSON metadata file is malformed or violates its schema.
    #[error("invalid dataset JSON at {path}: {source}")]
    Json {
        /// Malformed metadata path.
        path: PathBuf,
        /// JSON parser diagnostic.
        #[source]
        source: serde_json::Error,
    },
    /// `materials.json` has another schema or names an instance the scene lacks.
    #[error("invalid instance materials at {path}")]
    MaterialMetadata {
        /// Materials path.
        path: PathBuf,
    },
    /// Stored resolved configuration does not match the run manifest.
    #[error("resolved configuration fingerprint does not match run.json")]
    ResolvedConfigFingerprint,
    /// A committed scene path is not a canonical directory in the run domain.
    #[error("invalid committed scene path {path}")]
    InvalidScenePath {
        /// Rejected filesystem path.
        path: PathBuf,
    },
    /// The dataset does not contain the complete requested scene domain.
    #[error("dataset contains {actual} scenes; expected {expected}")]
    IncompleteDataset {
        /// Scene count declared by the run manifest.
        expected: u64,
        /// Canonical committed scene directories found.
        actual: u64,
    },
    /// A checksum manifest belongs to another schema or scene.
    #[error("checksum manifest identity is invalid at {path}")]
    ChecksumManifestIdentity {
        /// Invalid checksum manifest path.
        path: PathBuf,
    },
    /// A checksum manifest contains a path that could escape its scene.
    #[error("unsafe path in checksum manifest: {path}")]
    UnsafeChecksumPath {
        /// Rejected relative path.
        path: PathBuf,
    },
    /// Stored checksum entries and committed content files differ.
    #[error("stored checksums do not cover exactly the content below {path}")]
    FileSetMismatch {
        /// Scene directory with inconsistent coverage.
        path: PathBuf,
    },
    /// A committed file no longer matches its stored digest.
    #[error("checksum mismatch at {path}")]
    ChecksumMismatch {
        /// Modified or corrupt content file.
        path: PathBuf,
    },
    /// Stored scene metadata names another scene.
    #[error("scene metadata identity is invalid at {path}")]
    SceneMetadataIdentity {
        /// Invalid scene metadata path.
        path: PathBuf,
    },
    /// A frame directory name is not canonical.
    #[error("invalid frame path {path}")]
    InvalidFramePath {
        /// Rejected filesystem path.
        path: PathBuf,
    },
    /// Stored frame metadata names another camera.
    #[error("camera metadata identity is invalid at {path}")]
    CameraMetadataIdentity {
        /// Invalid frame metadata path.
        path: PathBuf,
    },
    /// Cameras do not expose one consistent nonempty channel set.
    #[error("inconsistent or empty render channels at {path}")]
    InconsistentChannels {
        /// Frame directory exposing the inconsistency.
        path: PathBuf,
    },
    /// A committed scene has no camera frames.
    #[error("committed scene has no frames at {path}")]
    EmptyFrames {
        /// Empty frames directory.
        path: PathBuf,
    },
    /// A platform count could not be represented safely.
    #[error("dataset file count overflow")]
    CountOverflow,
    /// An artifact could not be read or checksum output could not be written.
    #[error("dataset validation I/O failed at {path}: {source}")]
    Io {
        /// Artifact path.
        path: PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// A PNG artifact is malformed.
    #[error("invalid PNG artifact {path}: {message}")]
    Png {
        /// Artifact path.
        path: PathBuf,
        /// Decoder diagnostic.
        message: String,
    },
    /// An EXR artifact is malformed.
    #[error("invalid EXR artifact {path}: {message}")]
    Exr {
        /// Artifact path.
        path: PathBuf,
        /// Decoder diagnostic.
        message: String,
    },
    /// Encoded dimensions differ from the camera contract.
    #[error("artifact {path} dimensions are {actual:?}; expected {expected:?}")]
    Dimensions {
        /// Artifact path.
        path: PathBuf,
        /// Contract dimensions.
        expected: [u32; 2],
        /// Encoded dimensions.
        actual: [u32; 2],
    },
    /// Encoded PNG channel representation is not canonical.
    #[error("PNG artifact {path} has noncanonical format {actual}; expected {expected}")]
    PngFormat {
        /// Artifact path.
        path: PathBuf,
        /// Canonical channel representation.
        expected: String,
        /// Encoded channel representation.
        actual: String,
    },
    /// Depth contains a negative or non-finite metric value.
    #[error("depth artifact {path} has invalid value at pixel {index}")]
    InvalidDepth {
        /// Artifact path.
        path: PathBuf,
        /// Invalid row-major pixel index.
        index: usize,
    },
    /// Instance output contains an ID absent from the scene snapshot.
    #[error("instance artifact {path} contains undeclared ID {instance_id} at pixel {index}")]
    IllegalInstanceId {
        /// Artifact path.
        path: PathBuf,
        /// Invalid row-major pixel index.
        index: usize,
        /// Undeclared visible identifier.
        instance_id: u16,
    },
    /// Deterministic checksum JSON serialization failed.
    #[error("could not serialize scene checksum manifest: {0}")]
    Serialize(#[from] serde_json::Error),
}

fn validate_png(
    path: &Path,
    width: u32,
    height: u32,
    color: png::ColorType,
    depth: png::BitDepth,
    legal_ids: Option<&BTreeSet<u16>>,
) -> Result<(), DatasetValidationError> {
    let file = File::open(path).map_err(|source| io_error(path, source))?;
    let mut reader = png::Decoder::new(BufReader::new(file))
        .read_info()
        .map_err(|error| DatasetValidationError::Png {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
    let info = reader.info();
    if [info.width, info.height] != [width, height] {
        return Err(DatasetValidationError::Dimensions {
            path: path.to_owned(),
            expected: [width, height],
            actual: [info.width, info.height],
        });
    }
    if info.color_type != color || info.bit_depth != depth {
        return Err(DatasetValidationError::PngFormat {
            path: path.to_owned(),
            expected: format!("{color:?}/{depth:?}"),
            actual: format!("{:?}/{:?}", info.color_type, info.bit_depth),
        });
    }
    let mut bytes = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or_else(|| DatasetValidationError::Png {
                path: path.to_owned(),
                message: "decoded size overflow".to_owned()
            })?
    ];
    let output = reader
        .next_frame(&mut bytes)
        .map_err(|error| DatasetValidationError::Png {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
    if let Some(legal_ids) = legal_ids {
        for (index, chunk) in bytes[..output.buffer_size()].chunks_exact(2).enumerate() {
            let instance_id = u16::from_be_bytes([chunk[0], chunk[1]]);
            if !legal_ids.contains(&instance_id) {
                return Err(DatasetValidationError::IllegalInstanceId {
                    path: path.to_owned(),
                    index,
                    instance_id,
                });
            }
        }
    }
    Ok(())
}

fn validate_depth(path: &Path, width: u32, height: u32) -> Result<(), DatasetValidationError> {
    read_depth(path, width, height).map(drop)
}

/// Decodes a checked depth frame into row-major camera-Z metres.
pub(crate) fn read_depth(
    path: &Path,
    width: u32,
    height: u32,
) -> Result<Vec<f32>, DatasetValidationError> {
    let row_width = usize::try_from(width).map_err(|_| DatasetValidationError::Dimensions {
        path: path.to_owned(),
        expected: [width, height],
        actual: [u32::MAX, height],
    })?;
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .specific_channels()
        .required("Z")
        .collect_pixels(
            |resolution, _| vec![0.0_f32; resolution.area()],
            move |samples, position, (value,): (f32,)| {
                samples[position.y() * row_width + position.x()] = value;
            },
        )
        .first_valid_layer()
        .all_attributes()
        .from_file(path)
        .map_err(|error| DatasetValidationError::Exr {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
    let actual = [
        u32::try_from(image.layer_data.size.width()).unwrap_or(u32::MAX),
        u32::try_from(image.layer_data.size.height()).unwrap_or(u32::MAX),
    ];
    if actual != [width, height] {
        return Err(DatasetValidationError::Dimensions {
            path: path.to_owned(),
            expected: [width, height],
            actual,
        });
    }
    if let Some((index, _)) = image
        .layer_data
        .channel_data
        .pixels
        .iter()
        .enumerate()
        .find(|(_, value)| !value.is_finite() || **value < 0.0)
    {
        return Err(DatasetValidationError::InvalidDepth {
            path: path.to_owned(),
            index,
        });
    }
    Ok(image.layer_data.channel_data.pixels)
}

#[derive(Serialize)]
struct ChecksumManifest {
    schema_version: u16,
    scene_id: u64,
    files: BTreeMap<String, String>,
}

fn write_checksums(
    staging: &SceneStagingArea,
    scene_id: SceneId,
    request: &RenderSceneRequest,
) -> Result<PathBuf, DatasetValidationError> {
    let mut paths = vec![staging.scene_metadata_path()];
    if !request.instance_materials().is_empty() {
        paths.push(staging.materials_path());
    }
    for camera in request.cameras() {
        paths.push(staging.frame_metadata_path(camera.camera_id()));
        if request.required_channels().rgb() {
            paths.push(staging.rgb_path(camera.camera_id()));
        }
        if request.required_channels().depth() {
            paths.push(staging.depth_path(camera.camera_id()));
        }
        if request.required_channels().instance() {
            paths.push(staging.instance_path(camera.camera_id()));
        }
    }
    let mut files = BTreeMap::new();
    for path in paths {
        let bytes = fs::read(&path).map_err(|source| io_error(&path, source))?;
        let relative = path
            .strip_prefix(staging.root())
            .expect("staging child")
            .to_string_lossy()
            .replace('\\', "/");
        files.insert(relative, blake3::hash(&bytes).to_hex().to_string());
    }
    let manifest = ChecksumManifest {
        schema_version: 1,
        scene_id: scene_id.get(),
        files,
    };
    let path = staging.root().join("checksums.json");
    let mut bytes = serde_json::to_vec(&manifest)?;
    bytes.push(b'\n');
    let mut file = File::create(&path).map_err(|source| io_error(&path, source))?;
    // Made durable together with the rest of the scene by `SceneStagingArea::commit`.
    file.write_all(&bytes)
        .map_err(|source| io_error(&path, source))?;
    Ok(path)
}

fn io_error(path: &Path, source: std::io::Error) -> DatasetValidationError {
    DatasetValidationError::Io {
        path: path.to_owned(),
        source,
    }
}

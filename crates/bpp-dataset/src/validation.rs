use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{BufReader, Write},
    path::{Path, PathBuf},
};

use bpp_core::{RenderSceneRequest, SceneId};
use exr::prelude::{ReadChannels as _, ReadLayers as _, ReadSpecificChannel as _, read};
use serde::Serialize;
use thiserror::Error;

use crate::SceneStagingArea;

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
    Ok(())
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
    file.write_all(&bytes)
        .map_err(|source| io_error(&path, source))?;
    file.sync_all().map_err(|source| io_error(&path, source))?;
    Ok(path)
}

fn io_error(path: &Path, source: std::io::Error) -> DatasetValidationError {
    DatasetValidationError::Io {
        path: path.to_owned(),
        source,
    }
}

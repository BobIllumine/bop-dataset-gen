//! Equirectangular HDR environment maps for image-based lighting.

use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

use thiserror::Error;

/// Largest accepted environment image edge.
const MAX_EDGE: u32 = 16_384;

/// A decoded equirectangular environment in linear RGB radiance, rows top
/// (zenith) to bottom (nadir), the left edge facing world -X (as Blender maps it).
#[derive(Clone, Debug, PartialEq)]
pub struct EnvironmentMap {
    source_path: PathBuf,
    content_fingerprint: [u8; 32],
    width: u32,
    height: u32,
    rgb: Vec<f32>,
}

impl EnvironmentMap {
    /// Returns the file the map was loaded from.
    #[must_use]
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    /// Returns the BLAKE3 hash of the file's bytes.
    #[must_use]
    pub const fn content_fingerprint(&self) -> &[u8; 32] {
        &self.content_fingerprint
    }

    /// Returns `[width, height]` in texels.
    #[must_use]
    pub const fn dimensions(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    /// Returns row-major linear RGB radiance, three floats per texel.
    #[must_use]
    pub fn rgb(&self) -> &[f32] {
        &self.rgb
    }
}

/// Why an environment map cannot be loaded.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EnvironmentLoadError {
    /// The file name does not end in `.hdr` or `.exr`.
    #[error("environment maps must be .hdr or .exr files")]
    UnsupportedFormat,
    /// The Radiance HDR decoder rejected the file.
    #[error("cannot decode HDR: {0}")]
    Hdr(#[from] image::ImageError),
    /// The `OpenEXR` decoder rejected the file.
    #[error("cannot decode EXR: {0}")]
    Exr(#[from] exr::error::Error),
    /// The image is empty, too large, or not twice as wide as it is tall.
    #[error("environment map is {width}x{height}; it must be 2:1 and at most {MAX_EDGE} wide")]
    Dimensions {
        /// Width in texels.
        width: u32,
        /// Height in texels.
        height: u32,
    },
    /// A texel is negative, NaN or infinite.
    #[error("environment map texel {index} is not a finite nonnegative radiance")]
    InvalidRadiance {
        /// Index of the first bad float.
        index: usize,
    },
}

/// Decodes an equirectangular `.hdr` or `.exr` environment from `bytes`;
/// `path` names it and selects the format.
///
/// # Errors
///
/// Returns [`EnvironmentLoadError`] for an unsupported format, a decoding
/// failure, a non-2:1 or oversized image, or invalid radiance.
pub fn load_environment(path: &Path, bytes: &[u8]) -> Result<EnvironmentMap, EnvironmentLoadError> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let (width, height, rgb) = match extension.as_deref() {
        Some("hdr") => {
            let image =
                image::load_from_memory_with_format(bytes, image::ImageFormat::Hdr)?.into_rgb32f();
            (image.width(), image.height(), image.into_raw())
        }
        Some("exr") => decode_exr(bytes)?,
        _ => return Err(EnvironmentLoadError::UnsupportedFormat),
    };
    if width == 0 || height == 0 || width > MAX_EDGE || width != 2 * height {
        return Err(EnvironmentLoadError::Dimensions { width, height });
    }
    if let Some(index) = rgb
        .iter()
        .position(|value| !value.is_finite() || *value < 0.0)
    {
        return Err(EnvironmentLoadError::InvalidRadiance { index });
    }
    Ok(EnvironmentMap {
        source_path: path.to_owned(),
        content_fingerprint: *blake3::hash(bytes).as_bytes(),
        width,
        height,
        rgb,
    })
}

fn decode_exr(bytes: &[u8]) -> Result<(u32, u32, Vec<f32>), EnvironmentLoadError> {
    use exr::prelude::{ReadChannels as _, ReadLayers as _, ReadSpecificChannel as _, read};
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .specific_channels()
        .required("R")
        .required("G")
        .required("B")
        .collect_pixels(
            |resolution, _| {
                (
                    resolution.width(),
                    vec![0.0_f32; resolution.width() * resolution.height() * 3],
                )
            },
            |(width, rgb): &mut (usize, Vec<f32>), position, (r, g, b): (f32, f32, f32)| {
                let index = (position.y() * *width + position.x()) * 3;
                rgb[index..index + 3].copy_from_slice(&[r, g, b]);
            },
        )
        .first_valid_layer()
        .all_attributes()
        .from_buffered(Cursor::new(bytes))?;
    let size = image.layer_data.size;
    let dimension = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
    Ok((
        dimension(size.width()),
        dimension(size.height()),
        image.layer_data.channel_data.pixels.1,
    ))
}

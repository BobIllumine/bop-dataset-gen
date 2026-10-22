//! Small images of stored frames, for browsing a dataset over a slow link.
//!
//! A frame's files are stored at full resolution and precision: a 1024×1024
//! depth EXR is about 2 MB. A preview is what a browser shows instead: the
//! frame scaled to fit a requested size, depth and instance labels already
//! coloured the way the studio colours them. RGB and depth become JPEG and
//! instance labels PNG, which keeps their edges exact.

use std::io::Cursor;

use exr::prelude::{ReadChannels as _, ReadLayers as _, ReadSpecificChannel as _, read};
use image::{
    ExtendedColorType, ImageFormat, RgbImage,
    codecs::{jpeg::JpegEncoder, png::PngEncoder},
    imageops::{self, FilterType},
};
use thiserror::Error;

/// Longest edge of a preview when the request names none.
pub const DEFAULT_SIZE: u32 = 512;
/// Smallest and largest longest edge a request may ask for.
pub const SIZES: std::ops::RangeInclusive<u32> = 16..=4096;
/// JPEG quality for RGB and depth previews.
const JPEG_QUALITY: u8 = 85;
/// Colour of background pixels in depth and instance previews.
const BACKGROUND: [u8; 3] = [11, 12, 14];

/// A stored frame file a preview can be made of.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameFile {
    /// `rgb.png`: 8-bit sRGB.
    Rgb,
    /// `depth.exr`: 32-bit float camera-Z metres.
    Depth,
    /// `instance.png`: 16-bit visible instance IDs.
    Instance,
}

impl FrameFile {
    /// The frame file a stored path names, or `None` for any other file.
    #[must_use]
    pub fn of(path: &str) -> Option<Self> {
        if !path.starts_with("scenes/") {
            return None;
        }
        match path.rsplit('/').next()? {
            "rgb.png" => Some(Self::Rgb),
            "depth.exr" => Some(Self::Depth),
            "instance.png" => Some(Self::Instance),
            _ => None,
        }
    }
}

/// An encoded preview image.
pub struct Preview {
    /// The image file.
    pub bytes: Vec<u8>,
    /// `image/jpeg` or `image/png`.
    pub content_type: &'static str,
}

/// A stored frame could not be read or its preview written.
#[derive(Debug, Error)]
pub enum PreviewError {
    /// The stored file is not the image its name says.
    #[error("cannot read the stored frame: {0}")]
    Decode(String),
    /// The preview could not be encoded.
    #[error("cannot encode the preview: {0}")]
    Encode(String),
}

/// Makes the preview of a stored frame file, scaled to fit `size` pixels on
/// its longest edge; a smaller frame keeps its size.
///
/// # Errors
///
/// Returns why the frame could not be read or the preview written.
pub fn render(file: FrameFile, stored: &[u8], size: u32) -> Result<Preview, PreviewError> {
    match file {
        FrameFile::Rgb => {
            let image = image::load_from_memory_with_format(stored, ImageFormat::Png)
                .map_err(|error| PreviewError::Decode(error.to_string()))?
                .into_rgb8();
            let (width, height) = fit(image.width(), image.height(), size);
            let scaled = if (width, height) == image.dimensions() {
                image
            } else {
                imageops::resize(&image, width, height, FilterType::Triangle)
            };
            jpeg(&scaled)
        }
        FrameFile::Depth => {
            let (width, height, metres) = read_depth(stored)?;
            let (near, far) = depth_range(&metres);
            let span = if far > near { far - near } else { 1.0 };
            let coloured = sample(width, height, size, |index| {
                let depth = metres[index];
                if depth > 0.0 {
                    turbo(1.0 - f64::from((depth - near) / span))
                } else {
                    BACKGROUND
                }
            });
            jpeg(&coloured)
        }
        FrameFile::Instance => {
            let labels = image::load_from_memory_with_format(stored, ImageFormat::Png)
                .map_err(|error| PreviewError::Decode(error.to_string()))?
                .into_luma16();
            let (width, height) = labels.dimensions();
            let ids = labels.into_raw();
            let coloured = sample(width, height, size, |index| match ids[index] {
                0 => BACKGROUND,
                id => label_color(id),
            });
            png(&coloured)
        }
    }
}

/// The size of a `width`×`height` frame scaled to fit `size` on its longest
/// edge, never enlarged and never empty.
fn fit(width: u32, height: u32, size: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= size {
        return (width, height);
    }
    let scale = |edge: u32| {
        u32::try_from(u64::from(edge) * u64::from(size) / u64::from(longest))
            .unwrap_or(u32::MAX)
            .max(1)
    };
    (scale(width), scale(height))
}

/// Colours a frame scaled to fit `size`, taking each preview pixel from the
/// frame pixel at its centre, so labels and depth are never blended.
fn sample(width: u32, height: u32, size: u32, colour: impl Fn(usize) -> [u8; 3]) -> RgbImage {
    let (preview_width, preview_height) = fit(width, height, size);
    let row = width as usize;
    RgbImage::from_fn(preview_width, preview_height, |x, y| {
        let source = |position: u32, preview: u32, frame: u32| {
            // Below `frame`, so it fits.
            let centre =
                (2 * u64::from(position) + 1) * u64::from(frame) / (2 * u64::from(preview));
            u32::try_from(centre).unwrap_or(frame - 1) as usize
        };
        let index = source(y, preview_height, height) * row + source(x, preview_width, width);
        image::Rgb(colour(index))
    })
}

/// Width, height, and row-major metres of a stored depth EXR.
fn read_depth(stored: &[u8]) -> Result<(u32, u32, Vec<f32>), PreviewError> {
    let decode = |error: exr::error::Error| PreviewError::Decode(error.to_string());
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .specific_channels()
        .required("Z")
        .collect_pixels(
            |resolution, _| (resolution.width(), vec![0.0_f32; resolution.area()]),
            |(width, samples): &mut (usize, Vec<f32>), position, (value,): (f32,)| {
                samples[position.y() * *width + position.x()] = value;
            },
        )
        .first_valid_layer()
        .all_attributes()
        .from_buffered(Cursor::new(stored))
        .map_err(decode)?;
    let size = image.layer_data.size;
    let dimension = |edge: usize| {
        u32::try_from(edge).map_err(|_| PreviewError::Decode("frame is too large".to_owned()))
    };
    Ok((
        dimension(size.width())?,
        dimension(size.height())?,
        image.layer_data.channel_data.pixels.1,
    ))
}

/// The nearest and farthest foreground depth; `(0, 1)` without foreground.
fn depth_range(metres: &[f32]) -> (f32, f32) {
    let (near, far) = metres
        .iter()
        .filter(|depth| **depth > 0.0)
        .fold((f32::INFINITY, 0.0_f32), |(near, far), &depth| {
            (near.min(depth), far.max(depth))
        });
    if near <= far { (near, far) } else { (0.0, 1.0) }
}

/// Turbo colormap (Mikhailov 2019), polynomial approximation, `t` in `[0, 1]`.
/// Matches the studio's, so previews and its own decoding look alike.
fn turbo(t: f64) -> [u8; 3] {
    let r = 0.1357 + t * (4.5974 - t * (42.3277 - t * (130.5887 - t * (150.5666 - t * 58.1375))));
    let g = 0.0914 + t * (2.1856 + t * (4.8052 - t * (14.0195 - t * (4.2109 + t * 2.7747))));
    let b = 0.1067 + t * (12.5925 - t * (60.1097 - t * (109.0745 - t * (88.5066 - t * 26.8183))));
    [r, g, b].map(channel)
}

/// A stable colour per instance ID, neighbouring IDs well apart. Matches the
/// studio's `labelColor`.
fn label_color(id: u16) -> [u8; 3] {
    let hue = (f64::from(id) * 0.618_033_988_749_895) % 1.0;
    let (saturation, lightness): (f64, f64) = (0.65, if id % 2 == 1 { 0.58 } else { 0.46 });
    let k = |n: f64| (n + hue * 12.0) % 12.0;
    let a = saturation * lightness.min(1.0 - lightness);
    let f = |n: f64| lightness - a * (k(n) - 3.0).min(9.0 - k(n)).clamp(-1.0, 1.0);
    [f(0.0), f(8.0), f(4.0)].map(channel)
}

/// A `[0, 1]` colour channel as a byte, out-of-range values clamped.
fn channel(value: f64) -> u8 {
    // In range after clamping, so the cast is exact.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the value is clamped to 0..=255 first"
    )]
    let byte = (255.0 * value.clamp(0.0, 1.0)).round() as u8;
    byte
}

fn jpeg(image: &RgbImage) -> Result<Preview, PreviewError> {
    let mut bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY)
        .encode(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgb8,
        )
        .map_err(|error| PreviewError::Encode(error.to_string()))?;
    Ok(Preview {
        bytes,
        content_type: "image/jpeg",
    })
}

fn png(image: &RgbImage) -> Result<Preview, PreviewError> {
    use image::ImageEncoder as _;
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgb8,
        )
        .map_err(|error| PreviewError::Encode(error.to_string()))?;
    Ok(Preview {
        bytes,
        content_type: "image/png",
    })
}

#[cfg(test)]
mod tests {
    use super::{FrameFile, fit, label_color, turbo};

    #[test]
    fn previews_fit_the_requested_size_without_enlarging() {
        assert_eq!(fit(1024, 1024, 512), (512, 512));
        assert_eq!(fit(640, 480, 320), (320, 240));
        assert_eq!(fit(640, 480, 1024), (640, 480));
        assert_eq!(fit(4000, 1, 16), (16, 1));
    }

    #[test]
    fn only_scene_frame_images_have_previews() {
        assert_eq!(
            FrameFile::of("scenes/00000000000000000001/frames/00001/depth.exr"),
            Some(FrameFile::Depth)
        );
        assert_eq!(
            FrameFile::of("scenes/1/frames/2/rgb.png"),
            Some(FrameFile::Rgb)
        );
        assert_eq!(FrameFile::of("scenes/1/scene.json"), None);
        assert_eq!(FrameFile::of("bop/depth.exr"), None);
    }

    #[test]
    fn colours_match_the_studio() {
        // Values computed with the studio's `turbo` and `labelColor`.
        assert_eq!(turbo(0.0), [35, 23, 27]);
        assert_eq!(turbo(0.5), [149, 251, 81]);
        assert_eq!(turbo(1.0), [144, 12, 0]);
        assert_eq!(label_color(1), [78, 119, 218]);
        assert_eq!(label_color(2), [130, 194, 41]);
        assert_eq!(label_color(7), [84, 218, 78]);
    }
}

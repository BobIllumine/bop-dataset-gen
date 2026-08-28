use std::{
    error::Error,
    fmt,
    fs::File,
    io::{self, BufWriter},
    path::Path,
};

use exr::prelude::{Image, SpecificChannels, Vec2, WritableImage};

/// Why a renderer frame buffer is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameValidationError {
    /// At least one image dimension is zero.
    ZeroDimensions,
    /// The dimensions and channel count cannot be represented on this target.
    PixelCountOverflow,
    /// The supplied buffer does not contain exactly one complete frame.
    InvalidDataLength {
        /// Required number of elements.
        expected: usize,
        /// Supplied number of elements.
        actual: usize,
    },
    /// One metric-depth sample is NaN or infinite.
    NonFiniteDepth {
        /// Row-major sample index.
        index: usize,
    },
    /// One metric-depth sample is less than zero.
    NegativeDepth {
        /// Row-major sample index.
        index: usize,
    },
}

impl fmt::Display for FrameValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDimensions => formatter.write_str("frame dimensions must be nonzero"),
            Self::PixelCountOverflow => {
                formatter.write_str("frame element count exceeds the target address space")
            }
            Self::InvalidDataLength { expected, actual } => write!(
                formatter,
                "frame buffer has {actual} elements but requires exactly {expected}"
            ),
            Self::NonFiniteDepth { index } => {
                write!(formatter, "depth sample {index} is not finite")
            }
            Self::NegativeDepth { index } => {
                write!(formatter, "depth sample {index} is negative")
            }
        }
    }
}

impl Error for FrameValidationError {}

/// Failure while encoding or writing a validated frame.
#[derive(Debug)]
pub enum FrameWriteError {
    /// Filesystem I/O failed.
    Io(io::Error),
    /// PNG encoding failed.
    Png(png::EncodingError),
    /// `OpenEXR` encoding failed.
    Exr(exr::error::Error),
    /// A temporary encoding buffer could not be represented or allocated.
    BufferTooLarge,
}

impl fmt::Display for FrameWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "frame I/O failed: {error}"),
            Self::Png(error) => write!(formatter, "PNG encoding failed: {error}"),
            Self::Exr(error) => write!(formatter, "OpenEXR encoding failed: {error}"),
            Self::BufferTooLarge => formatter.write_str("frame encoding buffer is too large"),
        }
    }
}

impl Error for FrameWriteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Png(error) => Some(error),
            Self::Exr(error) => Some(error),
            Self::BufferTooLarge => None,
        }
    }
}

impl From<io::Error> for FrameWriteError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<png::EncodingError> for FrameWriteError {
    fn from(error: png::EncodingError) -> Self {
        Self::Png(error)
    }
}

impl From<exr::error::Error> for FrameWriteError {
    fn from(error: exr::error::Error) -> Self {
        Self::Exr(error)
    }
}

/// Validated row-major, tightly packed, eight-bit sRGB pixels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RgbFrame {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl RgbFrame {
    /// Validates a tightly packed RGB buffer.
    ///
    /// # Errors
    ///
    /// Returns a typed error for zero dimensions, size overflow, or a buffer
    /// whose length is not exactly `width * height * 3`.
    pub fn try_new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, FrameValidationError> {
        validate_length(width, height, 3, pixels.len())?;
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Writes a canonical eight-bit RGB PNG carrying an sRGB declaration.
    ///
    /// # Errors
    ///
    /// Returns an I/O or PNG encoding error.
    pub fn write_png(&self, path: &Path) -> Result<(), FrameWriteError> {
        let writer = BufWriter::new(File::create(path)?);
        let mut encoder = png::Encoder::new(writer, self.width, self.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
        encoder.write_header()?.write_image_data(&self.pixels)?;
        Ok(())
    }
}

/// Validated row-major linear depth values measured in metres.
#[derive(Clone, Debug, PartialEq)]
pub struct DepthFrame {
    width: u32,
    height: u32,
    metres: Vec<f32>,
}

impl DepthFrame {
    /// Validates a tightly packed metric-depth buffer.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid dimensions, length, or any negative
    /// or non-finite sample.
    pub fn try_new(
        width: u32,
        height: u32,
        metres: Vec<f32>,
    ) -> Result<Self, FrameValidationError> {
        validate_length(width, height, 1, metres.len())?;
        for (index, depth) in metres.iter().copied().enumerate() {
            if !depth.is_finite() {
                return Err(FrameValidationError::NonFiniteDepth { index });
            }
            if depth < 0.0 {
                return Err(FrameValidationError::NegativeDepth { index });
            }
        }
        Ok(Self {
            width,
            height,
            metres,
        })
    }

    /// Writes canonical linear metric depth as one 32-bit float `Z` channel.
    ///
    /// # Errors
    ///
    /// Returns an `OpenEXR` encoding or filesystem error.
    pub fn write_exr(&self, path: &Path) -> Result<(), FrameWriteError> {
        let width = usize::try_from(self.width).map_err(|_| FrameWriteError::BufferTooLarge)?;
        let height = usize::try_from(self.height).map_err(|_| FrameWriteError::BufferTooLarge)?;
        let pixels =
            SpecificChannels::build()
                .with_channel("Z")
                .with_pixel_fn(|position: Vec2<usize>| {
                    (self.metres[position.y() * width + position.x()],)
                });
        Image::from_channels((width, height), pixels)
            .write()
            .to_file(path)?;
        Ok(())
    }
}

/// Validated row-major visible-instance identifiers; zero denotes background.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstanceFrame {
    width: u32,
    height: u32,
    ids: Vec<u16>,
}

impl InstanceFrame {
    /// Validates a tightly packed visible-instance identifier buffer.
    ///
    /// # Errors
    ///
    /// Returns a typed error for invalid dimensions or length.
    pub fn try_new(width: u32, height: u32, ids: Vec<u16>) -> Result<Self, FrameValidationError> {
        validate_length(width, height, 1, ids.len())?;
        Ok(Self { width, height, ids })
    }

    /// Writes identifiers losslessly as a sixteen-bit grayscale PNG.
    ///
    /// # Errors
    ///
    /// Returns an I/O, allocation, or PNG encoding error.
    pub fn write_png(&self, path: &Path) -> Result<(), FrameWriteError> {
        let byte_count = self
            .ids
            .len()
            .checked_mul(2)
            .ok_or(FrameWriteError::BufferTooLarge)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_count)
            .map_err(|_| FrameWriteError::BufferTooLarge)?;
        for id in &self.ids {
            bytes.extend_from_slice(&id.to_be_bytes());
        }

        let writer = BufWriter::new(File::create(path)?);
        let mut encoder = png::Encoder::new(writer, self.width, self.height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Sixteen);
        encoder.write_header()?.write_image_data(&bytes)?;
        Ok(())
    }
}

fn validate_length(
    width: u32,
    height: u32,
    channels: u64,
    actual: usize,
) -> Result<(), FrameValidationError> {
    if width == 0 || height == 0 {
        return Err(FrameValidationError::ZeroDimensions);
    }
    let expected = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(channels))
        .and_then(|elements| usize::try_from(elements).ok())
        .ok_or(FrameValidationError::PixelCountOverflow)?;
    if actual != expected {
        return Err(FrameValidationError::InvalidDataLength { expected, actual });
    }
    Ok(())
}

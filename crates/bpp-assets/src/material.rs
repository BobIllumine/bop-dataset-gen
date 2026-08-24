use crate::{InvalidPbrMaterialError, InvalidTextureImageError};

/// Stable index into a normalized asset's material table.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MaterialIndex(u32);

impl MaterialIndex {
    /// Creates a material-table index.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the underlying table index.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Stable index into a normalized asset's texture table.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TextureIndex(u32);

impl TextureIndex {
    /// Creates a texture-table index.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the underlying table index.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Base-color texture selection for one material.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextureBinding {
    texture: TextureIndex,
    tex_coord_set: u32,
}

impl TextureBinding {
    /// Selects a texture and the primitive UV set used to sample it.
    #[must_use]
    pub const fn new(texture: TextureIndex, tex_coord_set: u32) -> Self {
        Self {
            texture,
            tex_coord_set,
        }
    }

    /// Returns the selected texture-table index.
    #[must_use]
    pub const fn texture(self) -> TextureIndex {
        self.texture
    }

    /// Returns the selected texture-coordinate set.
    #[must_use]
    pub const fn tex_coord_set(self) -> u32 {
        self.tex_coord_set
    }
}

/// Checked opaque metallic-roughness material.
///
/// The base-color factor contains linear RGBA multipliers. A bound base-color
/// texture stores sRGB RGB texels and linear alpha separately in its RGBA8
/// image. Version 1 retains alpha bytes but renders assets as opaque.
#[derive(Clone, Debug, PartialEq)]
pub struct PbrMaterial {
    base_color_factor: [f32; 4],
    metallic_factor: f32,
    roughness_factor: f32,
    base_color_texture: Option<TextureBinding>,
}

impl PbrMaterial {
    /// Creates a checked opaque metallic-roughness material.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidPbrMaterialError`] when any factor is non-finite or
    /// outside the inclusive unit interval.
    pub fn try_new(
        base_color_factor: [f32; 4],
        metallic_factor: f32,
        roughness_factor: f32,
        base_color_texture: Option<TextureBinding>,
    ) -> Result<Self, InvalidPbrMaterialError> {
        for (component, value) in base_color_factor.iter().enumerate() {
            if !is_unit(*value) {
                return Err(InvalidPbrMaterialError::InvalidBaseColorComponent { component });
            }
        }
        if !is_unit(metallic_factor) {
            return Err(InvalidPbrMaterialError::InvalidMetallicFactor);
        }
        if !is_unit(roughness_factor) {
            return Err(InvalidPbrMaterialError::InvalidRoughnessFactor);
        }
        Ok(Self {
            base_color_factor,
            metallic_factor,
            roughness_factor,
            base_color_texture,
        })
    }

    /// Returns the linear RGBA base-color multiplier.
    #[must_use]
    pub const fn base_color_factor(&self) -> [f32; 4] {
        self.base_color_factor
    }

    /// Returns the metallic factor in `[0, 1]`.
    #[must_use]
    pub const fn metallic_factor(&self) -> f32 {
        self.metallic_factor
    }

    /// Returns the roughness factor in `[0, 1]`.
    #[must_use]
    pub const fn roughness_factor(&self) -> f32 {
        self.roughness_factor
    }

    /// Returns the optional base-color texture binding.
    #[must_use]
    pub const fn base_color_texture(&self) -> Option<TextureBinding> {
        self.base_color_texture
    }
}

/// Decoded two-dimensional RGBA8 texture image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextureImage {
    width: u32,
    height: u32,
    rgba8_srgb: Vec<u8>,
}

impl TextureImage {
    /// Creates a texture whose byte count exactly matches its dimensions.
    ///
    /// RGB bytes use the sRGB transfer function; alpha is linear and texels
    /// are not premultiplied.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidTextureImageError`] for zero dimensions, overflow, or
    /// an inconsistent RGBA byte count.
    pub fn try_new(
        width: u32,
        height: u32,
        rgba8_srgb: Vec<u8>,
    ) -> Result<Self, InvalidTextureImageError> {
        if width == 0 {
            return Err(InvalidTextureImageError::ZeroWidth);
        }
        if height == 0 {
            return Err(InvalidTextureImageError::ZeroHeight);
        }
        let expected = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(InvalidTextureImageError::PixelCountOverflow)?;
        if rgba8_srgb.len() != expected {
            return Err(InvalidTextureImageError::PixelLengthMismatch {
                expected,
                actual: rgba8_srgb.len(),
            });
        }
        Ok(Self {
            width,
            height,
            rgba8_srgb,
        })
    }

    /// Returns `[width, height]` in texels.
    #[must_use]
    pub const fn dimensions(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    /// Returns decoded, non-premultiplied RGBA8 texels in row-major order.
    #[must_use]
    pub fn rgba8_srgb(&self) -> &[u8] {
        &self.rgba8_srgb
    }
}

/// Texture magnification filter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MagnificationFilter {
    /// Select the nearest texel.
    Nearest,
    /// Linearly interpolate neighboring texels.
    Linear,
}

/// Texture minification filter, including glTF mipmap combinations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MinificationFilter {
    /// Select the nearest texel without mipmaps.
    Nearest,
    /// Linearly interpolate texels without mipmaps.
    Linear,
    /// Nearest texel from the nearest mip level.
    NearestMipmapNearest,
    /// Linear texel filtering from the nearest mip level.
    LinearMipmapNearest,
    /// Nearest texel filtering interpolated between mip levels.
    NearestMipmapLinear,
    /// Linear texel filtering interpolated between mip levels.
    LinearMipmapLinear,
}

/// Texture-coordinate wrapping mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WrapMode {
    /// Repeat the fractional texture coordinate.
    Repeat,
    /// Repeat while mirroring every other interval.
    MirroredRepeat,
    /// Clamp coordinates to the image edge.
    ClampToEdge,
}

/// Renderer-neutral texture sampling parameters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextureSampler {
    magnification: Option<MagnificationFilter>,
    minification: Option<MinificationFilter>,
    wrap_u: WrapMode,
    wrap_v: WrapMode,
}

impl TextureSampler {
    /// Creates explicit glTF-compatible sampler state.
    ///
    /// `None` preserves an unspecified filter for backend-appropriate automatic
    /// filtering. Wrapping is always explicit.
    #[must_use]
    pub const fn new(
        magnification: Option<MagnificationFilter>,
        minification: Option<MinificationFilter>,
        wrap_u: WrapMode,
        wrap_v: WrapMode,
    ) -> Self {
        Self {
            magnification,
            minification,
            wrap_u,
            wrap_v,
        }
    }

    /// Returns the optional magnification filter.
    #[must_use]
    pub const fn magnification(self) -> Option<MagnificationFilter> {
        self.magnification
    }

    /// Returns the optional minification filter.
    #[must_use]
    pub const fn minification(self) -> Option<MinificationFilter> {
        self.minification
    }

    /// Returns horizontal texture-coordinate wrapping.
    #[must_use]
    pub const fn wrap_u(self) -> WrapMode {
        self.wrap_u
    }

    /// Returns vertical texture-coordinate wrapping.
    #[must_use]
    pub const fn wrap_v(self) -> WrapMode {
        self.wrap_v
    }
}

impl Default for TextureSampler {
    fn default() -> Self {
        Self::new(None, None, WrapMode::Repeat, WrapMode::Repeat)
    }
}

/// Decoded texture image paired with renderer-neutral sampling state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Texture {
    image: TextureImage,
    sampler: TextureSampler,
}

impl Texture {
    /// Combines a checked texture image with its sampler state.
    #[must_use]
    pub const fn new(image: TextureImage, sampler: TextureSampler) -> Self {
        Self { image, sampler }
    }

    /// Returns the decoded texture image.
    #[must_use]
    pub const fn image(&self) -> &TextureImage {
        &self.image
    }

    /// Returns the texture sampler.
    #[must_use]
    pub const fn sampler(&self) -> TextureSampler {
        self.sampler
    }
}

fn is_unit(value: f32) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

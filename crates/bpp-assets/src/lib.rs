//! Bounded asset ingestion and renderer-neutral geometry types.

mod asset;
mod canonical;
mod errors;
mod geometry;
mod glb;
mod material;
mod stl;

pub use asset::NormalizedAsset;
pub use canonical::{
    CanonicalVisualAsset, CanonicalizeVisualAssetError, canonicalize_visual_asset,
};
pub use errors::{
    InvalidConvexPartError, InvalidNormalizedAssetError, InvalidPbrMaterialError,
    InvalidStlLoadOptionsError, InvalidTextureImageError, InvalidVisualMeshError,
    InvalidVisualPrimitiveError, StlLoadError,
};
pub use geometry::{ConvexPart, VisualMesh, VisualPrimitive};
pub use glb::{
    GlbLimitField, GlbLoadError, GlbLoadLimits, GlbMaterialFeature, InvalidGlbLoadLimitsError,
    LoadedVisualAsset, UnsupportedGlbFeature, load_glb,
};
pub use material::{
    MagnificationFilter, MaterialIndex, MinificationFilter, PbrMaterial, Texture, TextureBinding,
    TextureImage, TextureIndex, TextureSampler, WrapMode,
};
pub use stl::{StlLoadOptions, load_stl};

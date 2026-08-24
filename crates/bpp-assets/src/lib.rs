//! Bounded asset ingestion and renderer-neutral geometry types.

mod analytic_bin;
mod asset;
mod cache_key;
mod canonical;
mod decomposition;
mod errors;
mod geometry;
mod glb;
mod material;
mod stl;

pub use analytic_bin::{
    AnalyticBinCollision, AnalyticBinCollisionError, CollisionCuboid, analytic_bin_collision,
};
pub use asset::NormalizedAsset;
pub use cache_key::{AssetCacheKey, AssetCacheKeyInput, AssetSourceFormat, asset_cache_key};
pub use canonical::{
    CanonicalVisualAsset, CanonicalizeVisualAssetError, canonicalize_visual_asset,
};
pub use decomposition::{ConvexDecompositionError, VhacdResourceParameter, generate_convex_parts};
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

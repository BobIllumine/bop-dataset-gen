//! Bounded asset ingestion and renderer-neutral geometry types.

mod analytic_bin;
mod asset;
mod cache_key;
mod canonical;
mod catalog;
mod convex_cache;
mod decomposition;
mod environment;
mod errors;
mod geometry;
mod glb;
mod material;
mod optimize;
mod stl;

pub use analytic_bin::{
    AnalyticBinCollision, AnalyticBinCollisionError, CollisionCuboid, analytic_bin_collision,
    analytic_bin_snapshot,
};
pub use asset::NormalizedAsset;
pub use cache_key::{AssetCacheKey, AssetCacheKeyInput, AssetSourceFormat, asset_cache_key};
pub use canonical::{
    CanonicalVisualAsset, CanonicalizeVisualAssetError, canonicalize_visual_asset,
};
pub use catalog::{ResolvedAsset, ResolvedAssetCatalog, ResolvedAssetCatalogError};
pub use convex_cache::{
    CacheDisposition, CachedConvexParts, ConvexCacheIoOperation, ConvexDecompositionCache,
    ConvexDecompositionCacheError,
};
pub use decomposition::{ConvexDecompositionError, VhacdResourceParameter, generate_convex_parts};
pub use environment::{EnvironmentLoadError, EnvironmentMap, load_environment};
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
pub use optimize::{
    MeshOptimizationError, MeshOptimizationOptions, MeshOptimizationReport, OptimizedAsset,
    optimize_visual_asset,
};
pub use stl::{StlLoadOptions, load_stl};

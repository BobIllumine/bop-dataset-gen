//! Canonical asset ingestion and preprocessing.

mod asset;
mod errors;
mod geometry;
mod material;

pub use asset::NormalizedAsset;
pub use errors::{
    InvalidConvexPartError, InvalidNormalizedAssetError, InvalidPbrMaterialError,
    InvalidTextureImageError, InvalidVisualMeshError, InvalidVisualPrimitiveError,
};
pub use geometry::{ConvexPart, VisualMesh, VisualPrimitive};
pub use material::{
    MagnificationFilter, MaterialIndex, MinificationFilter, PbrMaterial, Texture, TextureBinding,
    TextureImage, TextureIndex, TextureSampler, WrapMode,
};

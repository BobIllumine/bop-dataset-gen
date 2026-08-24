//! Canonical asset ingestion and preprocessing.

mod asset;
mod errors;
mod geometry;
mod material;
mod stl;

pub use asset::NormalizedAsset;
pub use errors::{
    InvalidConvexPartError, InvalidNormalizedAssetError, InvalidPbrMaterialError,
    InvalidStlLoadOptionsError, InvalidTextureImageError, InvalidVisualMeshError,
    InvalidVisualPrimitiveError, StlLoadError,
};
pub use geometry::{ConvexPart, VisualMesh, VisualPrimitive};
pub use material::{
    MagnificationFilter, MaterialIndex, MinificationFilter, PbrMaterial, Texture, TextureBinding,
    TextureImage, TextureIndex, TextureSampler, WrapMode,
};
pub use stl::{StlLoadOptions, load_stl};

use thiserror::Error;

use crate::{MaterialIndex, TextureIndex};

/// Error returned when STL loading options do not define safe conversion bounds.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidStlLoadOptionsError {
    /// Source units cannot be converted to metres by a positive finite scale.
    #[error("STL source-unit scale must be positive and finite")]
    InvalidSourceUnitScale,
    /// No triangles are permitted by the configured resource limit.
    #[error("STL triangle limit must be nonzero")]
    ZeroMaxTriangles,
}

/// Error returned when STL input cannot be converted into a normalized visual mesh.
#[derive(Debug, Error, PartialEq)]
pub enum StlLoadError {
    /// The byte stream is not a well-formed ASCII or binary STL.
    #[error("malformed STL: {message}")]
    Malformed {
        /// Parser diagnostic with no parser type exposed in the public API.
        message: String,
    },
    /// The STL contains no triangles.
    #[error("STL contains no triangles")]
    EmptyMesh,
    /// The input contains more triangles than the caller permits.
    #[error("STL exceeds the configured limit of {limit} triangles")]
    TriangleLimitExceeded {
        /// Configured maximum triangle count.
        limit: usize,
    },
    /// A source vertex coordinate is NaN or infinite.
    #[error("STL triangle {triangle} vertex {vertex} component {component} is not finite")]
    NonFiniteCoordinate {
        /// Zero-based triangle index.
        triangle: usize,
        /// Zero-based vertex index within the triangle.
        vertex: usize,
        /// Zero-based coordinate component.
        component: usize,
    },
    /// Scaling a source coordinate into metres overflowed `f32`.
    #[error("scaled STL triangle {triangle} vertex {vertex} component {component} is not finite")]
    ScaledCoordinateNotFinite {
        /// Zero-based triangle index.
        triangle: usize,
        /// Zero-based vertex index within the triangle.
        vertex: usize,
        /// Zero-based coordinate component.
        component: usize,
    },
    /// Three source vertices do not define a finite, nonzero-area triangle.
    #[error("STL triangle {triangle} is degenerate")]
    DegenerateTriangle {
        /// Zero-based triangle index.
        triangle: usize,
    },
    /// Three vertices per triangle cannot be represented by the normalized `u32` index stream.
    #[error("STL vertex count exceeds u32 index capacity")]
    VertexCountOverflow,
    /// Generated geometry unexpectedly violated normalized primitive invariants.
    #[error("generated STL primitive is invalid: {0}")]
    InvalidPrimitive(#[from] InvalidVisualPrimitiveError),
    /// Generated geometry unexpectedly violated normalized mesh invariants.
    #[error("generated STL mesh is invalid: {0}")]
    InvalidMesh(#[from] InvalidVisualMeshError),
}

/// Error returned when a visual primitive violates normalized mesh invariants.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidVisualPrimitiveError {
    /// Fewer than three positions were supplied.
    #[error("visual primitive needs at least three positions, got {count}")]
    TooFewPositions {
        /// Supplied position count.
        count: usize,
    },
    /// The position and normal streams have different lengths.
    #[error("normal count {normals} does not match position count {positions}")]
    NormalCountMismatch {
        /// Position count.
        positions: usize,
        /// Normal count.
        normals: usize,
    },
    /// A position component is NaN or infinite.
    #[error("position {vertex} component {component} is not finite")]
    NonFinitePosition {
        /// Vertex index.
        vertex: usize,
        /// Component index.
        component: usize,
    },
    /// A normal component is NaN or infinite.
    #[error("normal {vertex} component {component} is not finite")]
    NonFiniteNormal {
        /// Vertex index.
        vertex: usize,
        /// Component index.
        component: usize,
    },
    /// A normal is zero length or cannot be normalized in `f32`.
    #[error("normal {vertex} cannot be normalized")]
    ZeroNormal {
        /// Vertex index.
        vertex: usize,
    },
    /// One texture-coordinate stream does not match the vertex count.
    #[error(
        "texture-coordinate set {set} has {tex_coords} entries but the primitive has {positions} positions"
    )]
    TexCoordCountMismatch {
        /// Texture-coordinate set index.
        set: usize,
        /// Position count.
        positions: usize,
        /// Texture-coordinate count.
        tex_coords: usize,
    },
    /// A texture-coordinate component is NaN or infinite.
    #[error("texture-coordinate set {set}, vertex {vertex}, component {component} is not finite")]
    NonFiniteTexCoord {
        /// Texture-coordinate set index.
        set: usize,
        /// Vertex index.
        vertex: usize,
        /// Component index.
        component: usize,
    },
    /// No triangle indices were supplied.
    #[error("visual primitive must contain triangle indices")]
    EmptyIndices,
    /// The index stream cannot be partitioned into triangles.
    #[error("visual primitive index count {count} is not divisible by three")]
    IndexCountNotMultipleOfThree {
        /// Supplied index count.
        count: usize,
    },
    /// An index references a vertex outside the position stream.
    #[error("index {index} at offset {index_offset} is outside vertex count {vertex_count}")]
    IndexOutOfBounds {
        /// Offset within the index stream.
        index_offset: usize,
        /// Invalid vertex index.
        index: u32,
        /// Available vertex count.
        vertex_count: usize,
    },
}

/// Error returned when a visual mesh has no primitives.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidVisualMeshError {
    /// The primitive collection is empty.
    #[error("visual mesh must contain at least one primitive")]
    EmptyPrimitives,
}

/// Error returned when PBR material factors are outside their defined ranges.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidPbrMaterialError {
    /// One linear base-color factor component is not finite or outside `[0, 1]`.
    #[error("base-color factor component {component} must be finite and within [0, 1]")]
    InvalidBaseColorComponent {
        /// Invalid RGBA component index.
        component: usize,
    },
    /// The metallic factor is not finite or outside `[0, 1]`.
    #[error("metallic factor must be finite and within [0, 1]")]
    InvalidMetallicFactor,
    /// The roughness factor is not finite or outside `[0, 1]`.
    #[error("roughness factor must be finite and within [0, 1]")]
    InvalidRoughnessFactor,
}

/// Error returned when decoded texture pixels do not match their dimensions.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidTextureImageError {
    /// Texture width is zero.
    #[error("texture width must be nonzero")]
    ZeroWidth,
    /// Texture height is zero.
    #[error("texture height must be nonzero")]
    ZeroHeight,
    /// Width, height, and four channels overflow the platform buffer length.
    #[error("texture RGBA pixel count overflows usize")]
    PixelCountOverflow,
    /// The decoded RGBA byte count does not equal `width * height * 4`.
    #[error("texture needs {expected} RGBA bytes but received {actual}")]
    PixelLengthMismatch {
        /// Required byte count.
        expected: usize,
        /// Supplied byte count.
        actual: usize,
    },
}

/// Error returned when a convex-part surface violates basic mesh invariants.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidConvexPartError {
    /// Fewer than four vertices were supplied.
    #[error("convex part needs at least four vertices, got {count}")]
    TooFewVertices {
        /// Supplied vertex count.
        count: usize,
    },
    /// A vertex component is NaN or infinite.
    #[error("convex vertex {vertex} component {component} is not finite")]
    NonFiniteVertex {
        /// Vertex index.
        vertex: usize,
        /// Component index.
        component: usize,
    },
    /// No triangle indices were supplied.
    #[error("convex part must contain triangle indices")]
    EmptyIndices,
    /// The index stream cannot be partitioned into triangles.
    #[error("convex-part index count {count} is not divisible by three")]
    IndexCountNotMultipleOfThree {
        /// Supplied index count.
        count: usize,
    },
    /// An index references a vertex outside the convex-part vertex stream.
    #[error("convex index {index} at offset {index_offset} is outside vertex count {vertex_count}")]
    IndexOutOfBounds {
        /// Offset within the index stream.
        index_offset: usize,
        /// Invalid vertex index.
        index: u32,
        /// Available vertex count.
        vertex_count: usize,
    },
}

/// Error returned when references inside a normalized asset are inconsistent.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidNormalizedAssetError {
    /// A primitive references a material outside the material table.
    #[error(
        "primitive {primitive} references material {material:?}, but only {material_count} materials exist"
    )]
    MaterialOutOfBounds {
        /// Primitive index.
        primitive: usize,
        /// Invalid material index.
        material: MaterialIndex,
        /// Available material count.
        material_count: usize,
    },
    /// A material references a texture outside the texture table.
    #[error(
        "material {material} references texture {texture:?}, but only {texture_count} textures exist"
    )]
    TextureOutOfBounds {
        /// Material index.
        material: usize,
        /// Invalid texture index.
        texture: TextureIndex,
        /// Available texture count.
        texture_count: usize,
    },
    /// A textured primitive does not provide the selected texture-coordinate set.
    #[error(
        "primitive {primitive} material {material} selects UV set {set}, but only {available} sets exist"
    )]
    MissingTexCoordSet {
        /// Primitive index.
        primitive: usize,
        /// Material-table index.
        material: usize,
        /// Missing texture-coordinate set.
        set: u32,
        /// Available texture-coordinate set count.
        available: usize,
    },
    /// No collision representation was supplied.
    #[error("normalized asset must contain at least one convex collision part")]
    EmptyConvexParts,
}

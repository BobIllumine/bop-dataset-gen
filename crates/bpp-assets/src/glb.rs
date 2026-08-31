use std::{
    io::Cursor,
    num::FpCategory,
    panic::{AssertUnwindSafe, catch_unwind},
};

use glam::Mat4;
use image::{ImageFormat, ImageReader, Limits};
use thiserror::Error;

use crate::{
    InvalidPbrMaterialError, InvalidTextureImageError, InvalidVisualMeshError,
    InvalidVisualPrimitiveError, MagnificationFilter, MaterialIndex, MinificationFilter,
    PbrMaterial, Texture, TextureBinding, TextureImage, TextureIndex, TextureSampler, VisualMesh,
    VisualPrimitive, WrapMode, geometry::validate_finite_positions,
};

/// A configurable resource dimension enforced during GLB ingestion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlbLimitField {
    /// Complete GLB container bytes.
    ContainerBytes,
    /// JSON chunk bytes.
    JsonBytes,
    /// Embedded BIN chunk bytes.
    BinaryBytes,
    /// Scene nodes visited from the selected scene.
    Nodes,
    /// Mesh primitives emitted after scene instancing.
    Primitives,
    /// Source material table entries.
    Materials,
    /// Source texture table entries.
    Textures,
    /// Source image table entries.
    Images,
    /// Vertex entries emitted across all primitives.
    Vertices,
    /// Triangle-list indices emitted across all primitives.
    Indices,
    /// Texture-coordinate sets on one primitive.
    TexCoordSets,
    /// Width or height of one decoded texture.
    TextureDimension,
    /// Encoded bytes for one embedded texture.
    EncodedImageBytes,
    /// RGBA bytes across all decoded textures.
    DecodedTextureBytes,
}

/// A glTF material feature outside the deliberately opaque v1 contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GlbMaterialFeature {
    /// Alpha masking or blending.
    AlphaMode,
    /// Two-sided rasterization and lighting.
    DoubleSided,
    /// Metallic-roughness texture channels.
    MetallicRoughnessTexture,
    /// Tangent-space normal mapping.
    NormalTexture,
    /// Ambient-occlusion texture sampling.
    OcclusionTexture,
    /// Emissive factor or texture output.
    Emissive,
}

/// Scene or geometry behavior outside the static v1 asset contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnsupportedGlbFeature {
    /// Per-vertex color multipliers.
    VertexColors,
    /// Tangent streams, which v1 cannot preserve without normal mapping.
    Tangents,
    /// Vertex joint-index streams.
    JointAttributes,
    /// Vertex joint-weight streams.
    WeightAttributes,
    /// Mesh morph targets.
    MorphTargets,
    /// Node or morph animation.
    Animation,
}

/// Error returned when a GLB resource limit is zero.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("GLB limit {field:?} must be nonzero")]
pub struct InvalidGlbLoadLimitsError {
    /// Invalid resource dimension.
    pub field: GlbLimitField,
}

/// Explicit resource bounds for self-contained GLB ingestion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GlbLoadLimits {
    container_bytes: usize,
    json_bytes: usize,
    binary_bytes: usize,
    nodes: usize,
    primitives: usize,
    materials: usize,
    textures: usize,
    images: usize,
    vertices: usize,
    indices: usize,
    tex_coord_sets: usize,
    texture_dimension: u32,
    encoded_image_bytes: usize,
    decoded_texture_bytes: usize,
}

impl GlbLoadLimits {
    const DEFAULT_CONTAINER_BYTES: usize = 64 * 1024 * 1024;
    const DEFAULT_JSON_BYTES: usize = 8 * 1024 * 1024;
    const DEFAULT_BINARY_BYTES: usize = 64 * 1024 * 1024;
    const DEFAULT_TABLE_ENTRIES: usize = 4096;
    const DEFAULT_TEX_COORD_SETS: usize = 4;

    /// Creates nonzero limits for geometry and texture ingestion.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidGlbLoadLimitsError`] naming the first zero limit.
    #[allow(
        clippy::too_many_arguments,
        reason = "every independent resource bound is mandatory"
    )]
    pub fn try_new(
        max_nodes: usize,
        max_primitives: usize,
        max_vertices: usize,
        max_indices: usize,
        max_texture_dimension: u32,
        max_encoded_image_bytes: usize,
        max_decoded_texture_bytes: usize,
    ) -> Result<Self, InvalidGlbLoadLimitsError> {
        for (value, field) in [
            (max_nodes, GlbLimitField::Nodes),
            (max_primitives, GlbLimitField::Primitives),
            (max_vertices, GlbLimitField::Vertices),
            (max_indices, GlbLimitField::Indices),
            (
                usize::try_from(max_texture_dimension).unwrap_or(usize::MAX),
                GlbLimitField::TextureDimension,
            ),
            (max_encoded_image_bytes, GlbLimitField::EncodedImageBytes),
            (
                max_decoded_texture_bytes,
                GlbLimitField::DecodedTextureBytes,
            ),
        ] {
            if value == 0 {
                return Err(InvalidGlbLoadLimitsError { field });
            }
        }
        Ok(Self {
            container_bytes: Self::DEFAULT_CONTAINER_BYTES,
            json_bytes: Self::DEFAULT_JSON_BYTES,
            binary_bytes: Self::DEFAULT_BINARY_BYTES,
            nodes: max_nodes,
            primitives: max_primitives,
            materials: Self::DEFAULT_TABLE_ENTRIES,
            textures: Self::DEFAULT_TABLE_ENTRIES,
            images: Self::DEFAULT_TABLE_ENTRIES,
            vertices: max_vertices,
            indices: max_indices,
            tex_coord_sets: Self::DEFAULT_TEX_COORD_SETS,
            texture_dimension: max_texture_dimension,
            encoded_image_bytes: max_encoded_image_bytes,
            decoded_texture_bytes: max_decoded_texture_bytes,
        })
    }

    /// Replaces finite container, table, and per-primitive UV-set limits.
    ///
    /// Defaults are intentionally conservative; callers handling larger
    /// trusted assets can opt into larger values explicitly.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidGlbLoadLimitsError`] naming the first zero limit.
    #[allow(
        clippy::too_many_arguments,
        reason = "each structural resource category is independently bounded"
    )]
    pub fn try_with_structural_limits(
        mut self,
        max_container_bytes: usize,
        max_json_bytes: usize,
        max_binary_bytes: usize,
        max_materials: usize,
        max_textures: usize,
        max_images: usize,
        max_tex_coord_sets: usize,
    ) -> Result<Self, InvalidGlbLoadLimitsError> {
        for (value, field) in [
            (max_container_bytes, GlbLimitField::ContainerBytes),
            (max_json_bytes, GlbLimitField::JsonBytes),
            (max_binary_bytes, GlbLimitField::BinaryBytes),
            (max_materials, GlbLimitField::Materials),
            (max_textures, GlbLimitField::Textures),
            (max_images, GlbLimitField::Images),
            (max_tex_coord_sets, GlbLimitField::TexCoordSets),
        ] {
            if value == 0 {
                return Err(InvalidGlbLoadLimitsError { field });
            }
        }
        self.container_bytes = max_container_bytes;
        self.json_bytes = max_json_bytes;
        self.binary_bytes = max_binary_bytes;
        self.materials = max_materials;
        self.textures = max_textures;
        self.images = max_images;
        self.tex_coord_sets = max_tex_coord_sets;
        Ok(self)
    }
}

/// Visual GLB content loaded before collision preprocessing.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedVisualAsset {
    visual_mesh: VisualMesh,
    materials: Vec<PbrMaterial>,
    textures: Vec<Texture>,
}

impl LoadedVisualAsset {
    /// Wraps a checked mesh that has no source material or texture tables.
    ///
    /// This is the canonical bridge for material-less formats such as STL.
    #[must_use]
    pub fn from_unmaterialized_mesh(visual_mesh: VisualMesh) -> Self {
        Self {
            visual_mesh,
            materials: Vec::new(),
            textures: Vec::new(),
        }
    }

    /// Returns scene-instanced visual geometry in metres in the GLB asset frame.
    #[must_use]
    pub const fn visual_mesh(&self) -> &VisualMesh {
        &self.visual_mesh
    }

    /// Returns preserved materials in stable table order.
    #[must_use]
    pub fn materials(&self) -> &[PbrMaterial] {
        &self.materials
    }

    /// Returns preserved decoded textures in stable table order.
    #[must_use]
    pub fn textures(&self) -> &[Texture] {
        &self.textures
    }

    pub(crate) fn into_parts(self) -> (VisualMesh, Vec<PbrMaterial>, Vec<Texture>) {
        (self.visual_mesh, self.materials, self.textures)
    }
}

/// Error returned when GLB input cannot be represented by the v1 visual contract.
#[derive(Debug, Error, PartialEq)]
pub enum GlbLoadError {
    /// Input is not a valid binary glTF 2.0 container.
    #[error("malformed GLB: {message}")]
    Malformed {
        /// Parser or structural diagnostic.
        message: String,
    },
    /// The third-party structural parser panicked on untrusted input and was contained.
    #[error("GLB parser panicked")]
    ParserPanicked,
    /// The third-party accessor layer panicked on untrusted data and was contained.
    #[error("GLB accessor processing panicked")]
    AccessorProcessingPanicked,
    /// The GLB has no embedded binary chunk.
    #[error("GLB must contain one embedded BIN chunk")]
    MissingBinaryChunk,
    /// A buffer references a resource outside the embedded BIN chunk.
    #[error("GLB buffer {buffer} is external")]
    ExternalBuffer {
        /// Source buffer index.
        buffer: usize,
    },
    /// The document describes a buffer table that cannot map unambiguously to one BIN chunk.
    #[error("GLB must describe exactly one embedded buffer, found {count}")]
    UnsupportedBufferLayout {
        /// Number of declared buffers.
        count: usize,
    },
    /// An image references a URI rather than the embedded BIN chunk.
    #[error("GLB image {image} is external")]
    ExternalImage {
        /// Source image index.
        image: usize,
    },
    /// The document declares an extension outside the v1 import contract.
    #[error("GLB uses unsupported extension {name}")]
    UnsupportedExtension {
        /// Declared extension name.
        name: String,
    },
    /// The document uses scene behavior absent from the static v1 contract.
    #[error("GLB uses unsupported feature {feature:?}")]
    UnsupportedFeature {
        /// Unsupported static-scene behavior.
        feature: UnsupportedGlbFeature,
    },
    /// An embedded image uses a format outside core PNG and JPEG support.
    #[error("GLB image {image} uses unsupported MIME type {mime_type}")]
    UnsupportedImageMimeType {
        /// Source image index.
        image: usize,
        /// Declared MIME type.
        mime_type: String,
    },
    /// Embedded image bytes cannot be decoded safely.
    #[error("GLB image {image} cannot be decoded: {message}")]
    ImageDecode {
        /// Source image index.
        image: usize,
        /// Decoder diagnostic.
        message: String,
    },
    /// The image decoder panicked on untrusted input and was contained.
    #[error("GLB image {image} decoder panicked")]
    ImageDecoderPanicked {
        /// Source image index.
        image: usize,
    },
    /// A material requires appearance data absent from the v1 contract.
    #[error("GLB material {material} uses unsupported feature {feature:?}")]
    UnsupportedMaterialFeature {
        /// Source material index.
        material: usize,
        /// Unsupported appearance behavior.
        feature: GlbMaterialFeature,
    },
    /// More than one scene exists without an explicit default.
    #[error("GLB has {count} scenes but no default scene")]
    AmbiguousScene {
        /// Scene count.
        count: usize,
    },
    /// The selected scene contains no visual primitives.
    #[error("GLB selected scene contains no mesh primitives")]
    EmptyScene,
    /// A configured resource limit was exceeded.
    #[error("GLB exceeds {field:?} limit {limit}")]
    LimitExceeded {
        /// Exceeded resource dimension.
        field: GlbLimitField,
        /// Configured maximum.
        limit: usize,
    },
    /// A scene node uses skinning, which v1 does not preserve.
    #[error("GLB node {node} uses unsupported skinning")]
    UnsupportedSkin {
        /// Source node index.
        node: usize,
    },
    /// A node transform is singular and cannot transform normals.
    #[error("GLB node {node} has a singular transform")]
    SingularNodeTransform {
        /// Source node index.
        node: usize,
    },
    /// A primitive topology is not yet representable as triangles.
    #[error("GLB primitive uses unsupported mode {mode:?}")]
    UnsupportedPrimitiveMode {
        /// Source glTF primitive mode.
        mode: gltf::mesh::Mode,
    },
    /// A primitive does not provide required position data.
    #[error("GLB primitive has no POSITION attribute")]
    MissingPositions,
    /// A declared accessor cannot be read from the embedded BIN chunk.
    #[error("GLB primitive {primitive} declared unreadable {semantic} data")]
    UnreadableAccessor {
        /// Emitted primitive index.
        primitive: usize,
        /// glTF attribute semantic or index stream name.
        semantic: String,
    },
    /// Texture-coordinate set numbering contains a gap.
    #[error(
        "GLB primitive {primitive} texture-coordinate sets are non-contiguous: expected {expected}, found {found}"
    )]
    NonContiguousTexCoordSets {
        /// Emitted primitive index.
        primitive: usize,
        /// Next required zero-based set.
        expected: u32,
        /// Set actually encountered.
        found: u32,
    },
    /// A triangle list has a trailing index that cannot form a complete triangle.
    #[error(
        "GLB primitive {primitive} has {count} triangle-list indices; count must be divisible by 3"
    )]
    InvalidTriangleIndexCount {
        /// Emitted primitive index.
        primitive: usize,
        /// Triangulated index count.
        count: usize,
    },
    /// A material texture binding names a UV set absent from its primitive.
    #[error(
        "GLB primitive {primitive} material {material} requires texture-coordinate set {set}, but only {available} contiguous sets are present"
    )]
    MissingTexCoordSet {
        /// Emitted primitive index.
        primitive: usize,
        /// Loaded material-table index.
        material: usize,
        /// Required texture-coordinate set.
        set: u32,
        /// Number of contiguous texture-coordinate sets on the primitive.
        available: usize,
    },
    /// A triangle has zero or non-finite area after intrinsic transforms.
    #[error("GLB primitive {primitive} triangle {triangle} is degenerate")]
    DegenerateTriangle {
        /// Emitted primitive index.
        primitive: usize,
        /// Triangle index within the primitive.
        triangle: usize,
    },
    /// Material construction unexpectedly violated normalized invariants.
    #[error("GLB material is invalid: {0}")]
    InvalidMaterial(#[from] InvalidPbrMaterialError),
    /// Decoded image construction unexpectedly violated texture invariants.
    #[error("GLB texture image is invalid: {0}")]
    InvalidTextureImage(#[from] InvalidTextureImageError),
    /// Geometry construction unexpectedly violated normalized primitive invariants.
    #[error("GLB primitive is invalid: {0}")]
    InvalidPrimitive(#[from] InvalidVisualPrimitiveError),
    /// Geometry construction unexpectedly violated normalized mesh invariants.
    #[error("GLB mesh is invalid: {0}")]
    InvalidMesh(#[from] InvalidVisualMeshError),
}

/// Loads a self-contained binary glTF scene in metres.
///
/// Intrinsic GLB node transforms are baked into emitted primitives. The glTF
/// Y-up asset frame is preserved for the later explicit model-to-canonical
/// transform step.
///
/// # Errors
///
/// Returns [`GlbLoadError`] for malformed, external, unsupported, empty, or
/// resource-exceeding input.
pub fn load_glb(bytes: &[u8], limits: GlbLoadLimits) -> Result<LoadedVisualAsset, GlbLoadError> {
    preflight_glb(bytes, limits)?;
    let binary = catch_unwind(AssertUnwindSafe(|| gltf::binary::Glb::from_slice(bytes)))
        .map_err(|_| GlbLoadError::ParserPanicked)?
        .map_err(|error| GlbLoadError::Malformed {
            message: error.to_string(),
        })?;
    let document = catch_unwind(AssertUnwindSafe(|| {
        let root = gltf::json::deserialize::from_slice(&binary.json)?;
        gltf::Document::from_json(root)
    }))
    .map_err(|_| GlbLoadError::ParserPanicked)?
    .map_err(|error| GlbLoadError::Malformed {
        message: error.to_string(),
    })?;
    catch_unwind(AssertUnwindSafe(|| {
        load_glb_document(&document, binary.bin.as_deref(), limits)
    }))
    .map_err(|_| GlbLoadError::AccessorProcessingPanicked)?
}

fn load_glb_document(
    document: &gltf::Document,
    binary_blob: Option<&[u8]>,
    limits: GlbLoadLimits,
) -> Result<LoadedVisualAsset, GlbLoadError> {
    enforce_document_tables(document, limits)?;
    if let Some(extension) = document.extensions_used().next() {
        return Err(GlbLoadError::UnsupportedExtension {
            name: extension.to_owned(),
        });
    }
    if document.animations().next().is_some() {
        return Err(GlbLoadError::UnsupportedFeature {
            feature: UnsupportedGlbFeature::Animation,
        });
    }
    let buffer_count = document.buffers().count();
    if buffer_count != 1 {
        return Err(GlbLoadError::UnsupportedBufferLayout {
            count: buffer_count,
        });
    }
    for buffer in document.buffers() {
        if !matches!(buffer.source(), gltf::buffer::Source::Bin) {
            return Err(GlbLoadError::ExternalBuffer {
                buffer: buffer.index(),
            });
        }
    }
    let blob = binary_blob.ok_or(GlbLoadError::MissingBinaryChunk)?;

    let scene = select_scene(document)?;
    let images = load_images(document, blob, limits)?;
    let textures = load_textures(document, &images);
    let mut materials = load_materials(document)?;
    let default_material_index =
        u32::try_from(materials.len()).map_err(|_| GlbLoadError::LimitExceeded {
            field: GlbLimitField::Primitives,
            limit: limits.primitives,
        })?;
    materials.push(PbrMaterial::try_new([1.0; 4], 1.0, 1.0, None)?);
    let primitives = load_scene_primitives(&scene, blob, default_material_index, limits)?;
    let visual_mesh = VisualMesh::try_new(primitives)?;
    validate_material_bindings(&visual_mesh, &materials)?;
    Ok(LoadedVisualAsset {
        visual_mesh,
        materials,
        textures,
    })
}

fn preflight_glb(bytes: &[u8], limits: GlbLoadLimits) -> Result<(), GlbLoadError> {
    enforce_available(
        bytes.len(),
        limits.container_bytes,
        limits.container_bytes,
        GlbLimitField::ContainerBytes,
    )?;
    if !bytes.starts_with(b"glTF") {
        return Err(GlbLoadError::Malformed {
            message: "missing binary glTF magic".to_owned(),
        });
    }
    let declared_length = bytes
        .get(8..12)
        .and_then(|value| <[u8; 4]>::try_from(value).ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| GlbLoadError::Malformed {
            message: "binary glTF header is shorter than 12 bytes".to_owned(),
        })?;
    if declared_length < 12 {
        return Err(GlbLoadError::Malformed {
            message: "declared binary glTF length is shorter than its header".to_owned(),
        });
    }
    if usize::try_from(declared_length).ok() != Some(bytes.len()) {
        return Err(GlbLoadError::Malformed {
            message: "declared binary glTF length does not match input length".to_owned(),
        });
    }
    let mut offset = 12_usize;
    while offset < bytes.len() {
        let chunk_length = read_u32(bytes, offset)?;
        let chunk_type = read_u32(bytes, offset + 4)?;
        let data_start = offset.checked_add(8).ok_or_else(chunk_layout_overflow)?;
        let data_end = data_start
            .checked_add(usize::try_from(chunk_length).unwrap_or(usize::MAX))
            .ok_or_else(chunk_layout_overflow)?;
        if data_end > bytes.len() {
            return Err(GlbLoadError::Malformed {
                message: "GLB chunk exceeds declared container length".to_owned(),
            });
        }
        let (field, limit) = match chunk_type {
            0x4e4f_534a => (GlbLimitField::JsonBytes, limits.json_bytes),
            0x004e_4942 => (GlbLimitField::BinaryBytes, limits.binary_bytes),
            _ => {
                offset = data_end;
                continue;
            }
        };
        enforce_available(data_end - data_start, limit, limit, field)?;
        offset = data_end;
    }
    Ok(())
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, GlbLoadError> {
    bytes
        .get(offset..offset.saturating_add(4))
        .and_then(|value| <[u8; 4]>::try_from(value).ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| GlbLoadError::Malformed {
            message: "GLB chunk header is truncated".to_owned(),
        })
}

fn chunk_layout_overflow() -> GlbLoadError {
    GlbLoadError::Malformed {
        message: "GLB chunk layout overflows usize".to_owned(),
    }
}

fn enforce_document_tables(
    document: &gltf::Document,
    limits: GlbLoadLimits,
) -> Result<(), GlbLoadError> {
    for (count, limit, field) in [
        (
            document.materials().count(),
            limits.materials,
            GlbLimitField::Materials,
        ),
        (
            document.textures().count(),
            limits.textures,
            GlbLimitField::Textures,
        ),
        (
            document.images().count(),
            limits.images,
            GlbLimitField::Images,
        ),
    ] {
        enforce_available(count, limit, limit, field)?;
    }
    Ok(())
}

fn load_textures(gltf: &gltf::Document, images: &[TextureImage]) -> Vec<Texture> {
    gltf.textures()
        .map(|texture| {
            let image = images[texture.source().index()].clone();
            Texture::new(image, convert_sampler(&texture.sampler()))
        })
        .collect()
}

fn load_materials(gltf: &gltf::Document) -> Result<Vec<PbrMaterial>, GlbLoadError> {
    gltf.materials()
        .map(|material| {
            validate_material(&material)?;
            let pbr = material.pbr_metallic_roughness();
            let base_color_texture = pbr.base_color_texture().map(|info| {
                TextureBinding::new(
                    TextureIndex::new(u32::try_from(info.texture().index()).unwrap_or(u32::MAX)),
                    info.tex_coord(),
                )
            });
            Ok(PbrMaterial::try_new(
                pbr.base_color_factor(),
                pbr.metallic_factor(),
                pbr.roughness_factor(),
                base_color_texture,
            )?)
        })
        .collect()
}

#[derive(Default)]
struct GeometryCounts {
    nodes: usize,
    vertices: usize,
    indices: usize,
}

fn load_scene_primitives(
    scene: &gltf::Scene<'_>,
    blob: &[u8],
    default_material_index: u32,
    limits: GlbLoadLimits,
) -> Result<Vec<VisualPrimitive>, GlbLoadError> {
    let mut primitives = Vec::new();
    let mut counts = GeometryCounts::default();
    let mut stack = Vec::new();
    let mut roots = Vec::new();
    for node in scene.nodes() {
        ensure_pending_node_capacity(counts.nodes, roots.len(), limits.nodes)?;
        roots.push(node);
    }
    for node in roots.into_iter().rev() {
        stack.push((node, Mat4::IDENTITY));
    }

    while let Some((node, parent_transform)) = stack.pop() {
        counts.nodes = checked_increment(counts.nodes, 1, limits.nodes, GlbLimitField::Nodes)?;
        if node.skin().is_some() {
            return Err(GlbLoadError::UnsupportedSkin { node: node.index() });
        }
        let local = Mat4::from_cols_array_2d(&node.transform().matrix());
        let world = parent_transform * local;
        let mut children = Vec::new();
        for child in node.children() {
            ensure_pending_node_capacity(counts.nodes, stack.len() + children.len(), limits.nodes)?;
            children.push(child);
        }
        for child in children.into_iter().rev() {
            stack.push((child, world));
        }
        let Some(mesh) = node.mesh() else {
            continue;
        };
        for primitive in mesh.primitives() {
            checked_increment(
                primitives.len(),
                1,
                limits.primitives,
                GlbLimitField::Primitives,
            )?;
            let loaded = load_primitive(
                &primitive,
                world,
                node.index(),
                blob,
                primitives.len(),
                default_material_index,
                GeometryBudget {
                    remaining_vertices: limits.vertices - counts.vertices,
                    remaining_indices: limits.indices - counts.indices,
                    vertex_limit: limits.vertices,
                    index_limit: limits.indices,
                    tex_coord_sets: limits.tex_coord_sets,
                },
            )?;
            counts.vertices += loaded.positions().len();
            counts.indices += loaded.indices().len();
            primitives.push(loaded);
        }
    }

    if primitives.is_empty() {
        return Err(GlbLoadError::EmptyScene);
    }
    Ok(primitives)
}

fn ensure_pending_node_capacity(
    visited: usize,
    pending: usize,
    limit: usize,
) -> Result<(), GlbLoadError> {
    visited
        .checked_add(pending)
        .and_then(|count| count.checked_add(1))
        .filter(|count| *count <= limit)
        .map(|_| ())
        .ok_or(GlbLoadError::LimitExceeded {
            field: GlbLimitField::Nodes,
            limit,
        })
}

#[derive(Clone, Copy)]
struct GeometryBudget {
    remaining_vertices: usize,
    remaining_indices: usize,
    vertex_limit: usize,
    index_limit: usize,
    tex_coord_sets: usize,
}

fn load_primitive(
    primitive: &gltf::Primitive<'_>,
    world: Mat4,
    node_index: usize,
    blob: &[u8],
    primitive_index: usize,
    default_material_index: u32,
    budget: GeometryBudget,
) -> Result<VisualPrimitive, GlbLoadError> {
    validate_primitive_features(primitive)?;
    let layout = validate_primitive_layout(primitive, primitive_index, budget)?;
    let (determinant, normal_transform) = checked_normal_transform(world, node_index)?;
    let reader = primitive.reader(|buffer| (buffer.index() == 0).then_some(blob));
    let positions = reader
        .read_positions()
        .ok_or_else(|| unreadable_accessor(primitive_index, "POSITION"))?
        .map(|position| world.transform_point3(position.into()).to_array())
        .collect::<Vec<_>>();
    validate_finite_positions(&positions).map_err(GlbLoadError::InvalidPrimitive)?;
    let mut loaded_tex_coord_sets = Vec::with_capacity(layout.tex_coord_sets.len());
    for set in layout.tex_coord_sets {
        let tex_coords = reader
            .read_tex_coords(set)
            .ok_or_else(|| unreadable_accessor(primitive_index, &format!("TEXCOORD_{set}")))?;
        loaded_tex_coord_sets.push(tex_coords.into_f32().collect::<Vec<_>>());
    }
    let source_indices = if primitive.indices().is_some() {
        reader
            .read_indices()
            .ok_or_else(|| unreadable_accessor(primitive_index, "INDICES"))?
            .into_u32()
            .collect()
    } else {
        (0..positions.len())
            .map(u32::try_from)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| GlbLoadError::LimitExceeded {
                field: GlbLimitField::Indices,
                limit: budget.index_limit,
            })?
    };
    let mut indices = triangulate(primitive.mode(), source_indices);
    if !indices.len().is_multiple_of(3) {
        return Err(GlbLoadError::InvalidTriangleIndexCount {
            primitive: primitive_index,
            count: indices.len(),
        });
    }
    if determinant.is_sign_negative() {
        for triangle in indices.chunks_exact_mut(3) {
            triangle.swap(1, 2);
        }
    }
    let streams = if layout.has_normals {
        let source_normals = reader
            .read_normals()
            .ok_or_else(|| unreadable_accessor(primitive_index, "NORMAL"))?;
        PrimitiveStreams {
            normals: source_normals
                .map(|normal| normal_transform.transform_vector3(normal.into()).to_array())
                .collect(),
            positions,
            tex_coord_sets: loaded_tex_coord_sets,
            indices,
        }
    } else {
        expand_with_flat_normals(
            &positions,
            &loaded_tex_coord_sets,
            &indices,
            primitive_index,
        )?
    };
    let material_index = primitive
        .material()
        .index()
        .map_or(default_material_index, |index| {
            u32::try_from(index).unwrap_or(u32::MAX)
        });
    Ok(VisualPrimitive::try_new(
        streams.positions,
        streams.normals,
        streams.tex_coord_sets,
        streams.indices,
        Some(MaterialIndex::new(material_index)),
    )?)
}

struct PrimitiveLayout {
    tex_coord_sets: Vec<u32>,
    has_normals: bool,
}

fn validate_primitive_layout(
    primitive: &gltf::Primitive<'_>,
    primitive_index: usize,
    budget: GeometryBudget,
) -> Result<PrimitiveLayout, GlbLoadError> {
    let positions_accessor = primitive
        .get(&gltf::Semantic::Positions)
        .ok_or(GlbLoadError::MissingPositions)?;
    let position_count = positions_accessor.count();
    enforce_available(
        position_count,
        budget.remaining_vertices,
        budget.vertex_limit,
        GlbLimitField::Vertices,
    )?;
    let normals = primitive.get(&gltf::Semantic::Normals);
    if let Some(normals) = normals.as_ref()
        && normals.count() != position_count
    {
        return Err(GlbLoadError::InvalidPrimitive(
            InvalidVisualPrimitiveError::NormalCountMismatch {
                positions: position_count,
                normals: normals.count(),
            },
        ));
    }
    let tex_coord_sets = declared_tex_coord_sets(
        primitive,
        primitive_index,
        position_count,
        budget.tex_coord_sets,
    )?;
    let source_index_count = primitive
        .indices()
        .map_or(position_count, |accessor| accessor.count());
    let output_index_count = triangulated_index_count(primitive.mode(), source_index_count).ok_or(
        GlbLoadError::LimitExceeded {
            field: GlbLimitField::Indices,
            limit: budget.index_limit,
        },
    )?;
    enforce_available(
        output_index_count,
        budget.remaining_indices,
        budget.index_limit,
        GlbLimitField::Indices,
    )?;
    if normals.is_none() {
        enforce_available(
            output_index_count,
            budget.remaining_vertices,
            budget.vertex_limit,
            GlbLimitField::Vertices,
        )?;
    }

    Ok(PrimitiveLayout {
        tex_coord_sets,
        has_normals: normals.is_some(),
    })
}

fn checked_normal_transform(world: Mat4, node: usize) -> Result<(f32, Mat4), GlbLoadError> {
    let determinant = world.determinant();
    if determinant.classify() == FpCategory::Zero || !determinant.is_finite() {
        return Err(GlbLoadError::SingularNodeTransform { node });
    }
    Ok((determinant, world.inverse().transpose()))
}

fn validate_primitive_features(primitive: &gltf::Primitive<'_>) -> Result<(), GlbLoadError> {
    for (semantic, _) in primitive.attributes() {
        let feature = match semantic {
            gltf::Semantic::Colors(_) => Some(UnsupportedGlbFeature::VertexColors),
            gltf::Semantic::Tangents => Some(UnsupportedGlbFeature::Tangents),
            gltf::Semantic::Joints(_) => Some(UnsupportedGlbFeature::JointAttributes),
            gltf::Semantic::Weights(_) => Some(UnsupportedGlbFeature::WeightAttributes),
            gltf::Semantic::Positions | gltf::Semantic::Normals | gltf::Semantic::TexCoords(_) => {
                None
            }
        };
        if let Some(feature) = feature {
            return Err(GlbLoadError::UnsupportedFeature { feature });
        }
    }
    if primitive.clone().morph_targets().next().is_some() {
        return Err(GlbLoadError::UnsupportedFeature {
            feature: UnsupportedGlbFeature::MorphTargets,
        });
    }
    if !matches!(
        primitive.mode(),
        gltf::mesh::Mode::Triangles
            | gltf::mesh::Mode::TriangleStrip
            | gltf::mesh::Mode::TriangleFan
    ) {
        return Err(GlbLoadError::UnsupportedPrimitiveMode {
            mode: primitive.mode(),
        });
    }
    Ok(())
}

fn declared_tex_coord_sets(
    primitive: &gltf::Primitive<'_>,
    primitive_index: usize,
    position_count: usize,
    limit: usize,
) -> Result<Vec<u32>, GlbLoadError> {
    let mut sets = Vec::new();
    for (semantic, accessor) in primitive.attributes() {
        if let gltf::Semantic::TexCoords(set) = semantic {
            enforce_available(sets.len() + 1, limit, limit, GlbLimitField::TexCoordSets)?;
            sets.push((set, accessor.count()));
        }
    }
    sets.sort_unstable_by_key(|(set, _)| *set);
    for (expected, (found, count)) in (0_u32..).zip(&sets) {
        if expected != *found {
            return Err(GlbLoadError::NonContiguousTexCoordSets {
                primitive: primitive_index,
                expected,
                found: *found,
            });
        }
        if *count != position_count {
            return Err(GlbLoadError::InvalidPrimitive(
                InvalidVisualPrimitiveError::TexCoordCountMismatch {
                    set: usize::try_from(*found).unwrap_or(usize::MAX),
                    positions: position_count,
                    tex_coords: *count,
                },
            ));
        }
    }
    Ok(sets.into_iter().map(|(set, _)| set).collect())
}

fn unreadable_accessor(primitive: usize, semantic: &str) -> GlbLoadError {
    GlbLoadError::UnreadableAccessor {
        primitive,
        semantic: semantic.to_owned(),
    }
}

fn triangulated_index_count(mode: gltf::mesh::Mode, source_count: usize) -> Option<usize> {
    match mode {
        gltf::mesh::Mode::Triangles => Some(source_count),
        gltf::mesh::Mode::TriangleStrip | gltf::mesh::Mode::TriangleFan => {
            source_count.saturating_sub(2).checked_mul(3)
        }
        _ => None,
    }
}

fn enforce_available(
    count: usize,
    available: usize,
    limit: usize,
    field: GlbLimitField,
) -> Result<(), GlbLoadError> {
    if count > available {
        return Err(GlbLoadError::LimitExceeded { field, limit });
    }
    Ok(())
}

fn validate_material_bindings(
    visual_mesh: &VisualMesh,
    materials: &[PbrMaterial],
) -> Result<(), GlbLoadError> {
    for (primitive_index, primitive) in visual_mesh.primitives().iter().enumerate() {
        let Some(material) = primitive.material() else {
            continue;
        };
        let material_index = usize::try_from(material.get()).unwrap_or(usize::MAX);
        let Some(binding) = materials[material_index].base_color_texture() else {
            continue;
        };
        let set_index = usize::try_from(binding.tex_coord_set()).unwrap_or(usize::MAX);
        if set_index >= primitive.tex_coord_sets().len() {
            return Err(GlbLoadError::MissingTexCoordSet {
                primitive: primitive_index,
                material: material_index,
                set: binding.tex_coord_set(),
                available: primitive.tex_coord_sets().len(),
            });
        }
    }
    Ok(())
}

fn validate_material(material: &gltf::Material<'_>) -> Result<(), GlbLoadError> {
    let material_index = material.index().unwrap_or(usize::MAX);
    if material.alpha_mode() != gltf::material::AlphaMode::Opaque {
        return Err(GlbLoadError::UnsupportedMaterialFeature {
            material: material_index,
            feature: GlbMaterialFeature::AlphaMode,
        });
    }
    if material.double_sided() {
        return Err(GlbLoadError::UnsupportedMaterialFeature {
            material: material_index,
            feature: GlbMaterialFeature::DoubleSided,
        });
    }
    let pbr = material.pbr_metallic_roughness();
    if pbr.metallic_roughness_texture().is_some() {
        return Err(GlbLoadError::UnsupportedMaterialFeature {
            material: material_index,
            feature: GlbMaterialFeature::MetallicRoughnessTexture,
        });
    }
    if material.normal_texture().is_some() {
        return Err(GlbLoadError::UnsupportedMaterialFeature {
            material: material_index,
            feature: GlbMaterialFeature::NormalTexture,
        });
    }
    if material.occlusion_texture().is_some() {
        return Err(GlbLoadError::UnsupportedMaterialFeature {
            material: material_index,
            feature: GlbMaterialFeature::OcclusionTexture,
        });
    }
    if material.emissive_texture().is_some()
        || material
            .emissive_factor()
            .iter()
            .any(|component| component.classify() != FpCategory::Zero)
    {
        return Err(GlbLoadError::UnsupportedMaterialFeature {
            material: material_index,
            feature: GlbMaterialFeature::Emissive,
        });
    }
    Ok(())
}

fn load_images(
    gltf: &gltf::Document,
    blob: &[u8],
    limits: GlbLoadLimits,
) -> Result<Vec<TextureImage>, GlbLoadError> {
    let mut images = Vec::new();
    let mut decoded_bytes = 0_usize;
    for image in gltf.images() {
        let gltf::image::Source::View { view, mime_type } = image.source() else {
            return Err(GlbLoadError::ExternalImage {
                image: image.index(),
            });
        };
        let start = view.offset();
        let end = start
            .checked_add(view.length())
            .ok_or_else(|| GlbLoadError::ImageDecode {
                image: image.index(),
                message: "buffer-view range overflows usize".to_owned(),
            })?;
        let encoded = blob
            .get(start..end)
            .ok_or_else(|| GlbLoadError::ImageDecode {
                image: image.index(),
                message: "buffer view exceeds embedded BIN chunk".to_owned(),
            })?;
        if encoded.len() > limits.encoded_image_bytes {
            return Err(GlbLoadError::LimitExceeded {
                field: GlbLimitField::EncodedImageBytes,
                limit: limits.encoded_image_bytes,
            });
        }
        let format = match mime_type {
            "image/png" => ImageFormat::Png,
            "image/jpeg" => ImageFormat::Jpeg,
            other => {
                return Err(GlbLoadError::UnsupportedImageMimeType {
                    image: image.index(),
                    mime_type: other.to_owned(),
                });
            }
        };
        let dimensions = catch_unwind(AssertUnwindSafe(|| {
            ImageReader::with_format(Cursor::new(encoded), format).into_dimensions()
        }))
        .map_err(|_| GlbLoadError::ImageDecoderPanicked {
            image: image.index(),
        })?
        .map_err(|error| GlbLoadError::ImageDecode {
            image: image.index(),
            message: error.to_string(),
        })?;
        if dimensions.0 > limits.texture_dimension || dimensions.1 > limits.texture_dimension {
            return Err(GlbLoadError::LimitExceeded {
                field: GlbLimitField::TextureDimension,
                limit: usize::try_from(limits.texture_dimension).unwrap_or(usize::MAX),
            });
        }
        let rgba_bytes = usize::try_from(dimensions.0)
            .ok()
            .and_then(|width| {
                usize::try_from(dimensions.1)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(GlbLoadError::LimitExceeded {
                field: GlbLimitField::DecodedTextureBytes,
                limit: limits.decoded_texture_bytes,
            })?;
        decoded_bytes = checked_increment(
            decoded_bytes,
            rgba_bytes,
            limits.decoded_texture_bytes,
            GlbLimitField::DecodedTextureBytes,
        )?;
        let mut decode_limits = Limits::default();
        decode_limits.max_image_width = Some(limits.texture_dimension);
        decode_limits.max_image_height = Some(limits.texture_dimension);
        decode_limits.max_alloc =
            Some(u64::try_from(limits.decoded_texture_bytes).unwrap_or(u64::MAX));
        let rgba = catch_unwind(AssertUnwindSafe(|| {
            let mut reader = ImageReader::with_format(Cursor::new(encoded), format);
            reader.limits(decode_limits);
            reader.decode().map(|image| image.to_rgba8())
        }))
        .map_err(|_| GlbLoadError::ImageDecoderPanicked {
            image: image.index(),
        })?
        .map_err(|error| GlbLoadError::ImageDecode {
            image: image.index(),
            message: error.to_string(),
        })?;
        images.push(TextureImage::try_new(
            dimensions.0,
            dimensions.1,
            rgba.into_raw(),
        )?);
    }
    Ok(images)
}

fn convert_sampler(sampler: &gltf::texture::Sampler<'_>) -> TextureSampler {
    let magnification = sampler.mag_filter().map(|filter| match filter {
        gltf::texture::MagFilter::Nearest => MagnificationFilter::Nearest,
        gltf::texture::MagFilter::Linear => MagnificationFilter::Linear,
    });
    let minification = sampler.min_filter().map(|filter| match filter {
        gltf::texture::MinFilter::Nearest => MinificationFilter::Nearest,
        gltf::texture::MinFilter::Linear => MinificationFilter::Linear,
        gltf::texture::MinFilter::NearestMipmapNearest => MinificationFilter::NearestMipmapNearest,
        gltf::texture::MinFilter::LinearMipmapNearest => MinificationFilter::LinearMipmapNearest,
        gltf::texture::MinFilter::NearestMipmapLinear => MinificationFilter::NearestMipmapLinear,
        gltf::texture::MinFilter::LinearMipmapLinear => MinificationFilter::LinearMipmapLinear,
    });
    TextureSampler::new(
        magnification,
        minification,
        convert_wrap(sampler.wrap_s()),
        convert_wrap(sampler.wrap_t()),
    )
}

fn convert_wrap(mode: gltf::texture::WrappingMode) -> WrapMode {
    match mode {
        gltf::texture::WrappingMode::ClampToEdge => WrapMode::ClampToEdge,
        gltf::texture::WrappingMode::MirroredRepeat => WrapMode::MirroredRepeat,
        gltf::texture::WrappingMode::Repeat => WrapMode::Repeat,
    }
}

fn triangulate(mode: gltf::mesh::Mode, source: Vec<u32>) -> Vec<u32> {
    match mode {
        gltf::mesh::Mode::Triangles => source,
        gltf::mesh::Mode::TriangleStrip => source
            .windows(3)
            .enumerate()
            .flat_map(|(triangle, vertices)| {
                if triangle.is_multiple_of(2) {
                    [vertices[0], vertices[1], vertices[2]]
                } else {
                    [vertices[0], vertices[2], vertices[1]]
                }
            })
            .collect(),
        gltf::mesh::Mode::TriangleFan => source
            .get(1..)
            .unwrap_or_default()
            .windows(2)
            .flat_map(|vertices| [source[0], vertices[0], vertices[1]])
            .collect(),
        _ => Vec::new(),
    }
}

struct PrimitiveStreams {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    tex_coord_sets: Vec<Vec<[f32; 2]>>,
    indices: Vec<u32>,
}

fn expand_with_flat_normals(
    positions: &[[f32; 3]],
    tex_coord_sets: &[Vec<[f32; 2]>],
    indices: &[u32],
    primitive: usize,
) -> Result<PrimitiveStreams, GlbLoadError> {
    let mut expanded_positions = Vec::with_capacity(indices.len());
    let mut normals = Vec::with_capacity(indices.len());
    let mut expanded_tex_coord_sets = tex_coord_sets
        .iter()
        .map(|_| Vec::with_capacity(indices.len()))
        .collect::<Vec<_>>();
    let mut expanded_indices = Vec::with_capacity(indices.len());
    for (triangle, triangle_indices) in indices.chunks_exact(3).enumerate() {
        let mut vertices = [[0.0; 3]; 3];
        for (corner, index) in triangle_indices.iter().copied().enumerate() {
            vertices[corner] = usize::try_from(index)
                .ok()
                .and_then(|index| positions.get(index))
                .copied()
                .ok_or(GlbLoadError::InvalidPrimitive(
                    InvalidVisualPrimitiveError::IndexOutOfBounds {
                        index_offset: triangle * 3 + corner,
                        index,
                        vertex_count: positions.len(),
                    },
                ))?;
        }
        let [first, second, third] = vertices;
        let normal =
            flat_normal([first, second, third]).ok_or(GlbLoadError::DegenerateTriangle {
                primitive,
                triangle,
            })?;
        let first_index =
            u32::try_from(expanded_positions.len()).map_err(|_| GlbLoadError::LimitExceeded {
                field: GlbLimitField::Vertices,
                limit: u32::MAX as usize,
            })?;
        expanded_positions.extend([first, second, third]);
        normals.extend([normal; 3]);
        for (expanded, source) in expanded_tex_coord_sets.iter_mut().zip(tex_coord_sets) {
            for index in triangle_indices {
                let tex_coord = usize::try_from(*index)
                    .ok()
                    .and_then(|index| source.get(index))
                    .copied()
                    .ok_or(GlbLoadError::InvalidPrimitive(
                        InvalidVisualPrimitiveError::TexCoordCountMismatch {
                            set: 0,
                            positions: positions.len(),
                            tex_coords: source.len(),
                        },
                    ))?;
                expanded.push(tex_coord);
            }
        }
        expanded_indices.extend([first_index, first_index + 1, first_index + 2]);
    }
    Ok(PrimitiveStreams {
        positions: expanded_positions,
        normals,
        tex_coord_sets: expanded_tex_coord_sets,
        indices: expanded_indices,
    })
}

fn flat_normal(vertices: [[f32; 3]; 3]) -> Option<[f32; 3]> {
    let vertices = vertices.map(|vertex| vertex.map(f64::from));
    let first_edge = subtract(vertices[1], vertices[0]);
    let second_edge = subtract(vertices[2], vertices[0]);
    let cross = [
        first_edge[1] * second_edge[2] - first_edge[2] * second_edge[1],
        first_edge[2] * second_edge[0] - first_edge[0] * second_edge[2],
        first_edge[0] * second_edge[1] - first_edge[1] * second_edge[0],
    ];
    let squared_length = cross.iter().map(|value| value * value).sum::<f64>();
    if squared_length.classify() == FpCategory::Zero || !squared_length.is_finite() {
        return None;
    }
    let length = squared_length.sqrt();
    Some(cross.map(|value| normalized_f32(value / length)))
}

fn subtract(left: [f64; 3], right: [f64; 3]) -> [f64; 3] {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn normalized_f32(value: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "normalized components are finite within [-1, 1] and output meshes use f32"
    )]
    let value = value as f32;
    if value.classify() == FpCategory::Zero {
        0.0
    } else {
        value
    }
}

fn select_scene(gltf: &gltf::Document) -> Result<gltf::Scene<'_>, GlbLoadError> {
    if let Some(scene) = gltf.default_scene() {
        return Ok(scene);
    }
    let mut scenes = gltf.scenes();
    let first = scenes.next().ok_or(GlbLoadError::EmptyScene)?;
    if scenes.next().is_some() {
        return Err(GlbLoadError::AmbiguousScene {
            count: gltf.scenes().count(),
        });
    }
    Ok(first)
}

fn checked_increment(
    current: usize,
    increment: usize,
    limit: usize,
    field: GlbLimitField,
) -> Result<usize, GlbLoadError> {
    current
        .checked_add(increment)
        .filter(|value| *value <= limit)
        .ok_or(GlbLoadError::LimitExceeded { field, limit })
}

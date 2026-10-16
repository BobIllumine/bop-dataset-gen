//! Public preparation, initialization, and scene error types.

#[allow(
    clippy::wildcard_imports,
    reason = "submodules share the crate root's imports and helpers"
)]
use super::*;

#[derive(Debug, Error)]
pub(crate) enum WgpuSceneError {
    #[error("wgpu assets have not been prepared")]
    NotPrepared,
    #[error("scene references asset {asset_id:?} missing from the prepared GPU catalog")]
    MissingPreparedAsset { asset_id: bpp_core::AssetId },
    #[error("scene draw count exceeds the u32 instance-index domain")]
    DrawCountOverflow,
    #[error("image dimension {value} cannot be represented exactly as f32")]
    ImageDimensionPrecision { value: u32 },
    #[error("appearance exceeds the supported limit of {maximum} directional or point lights")]
    TooManyLights { maximum: usize },
    #[error("wgpu device was lost: {message}")]
    DeviceLost { message: String },
    #[error("wgpu device-loss state lock was poisoned")]
    DeviceLossStatePoisoned,
    #[error(transparent)]
    Readback(#[from] WgpuReadbackError),
}

/// GPU buffer role involved in a preparation limit failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GpuBufferKind {
    /// Interleaved position, normal, and first-UV vertex buffer.
    Vertex,
    /// Triangle-list `u32` index buffer.
    Index,
}

/// Failure to upload a resolved asset catalog.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum WgpuPreparationError {
    /// A collection count exceeded its fixed public/GPU representation.
    #[error("asset geometry count overflowed its fixed representation")]
    CountOverflow,
    /// One source buffer exceeds the selected device limit.
    #[error(
        "asset {asset_id:?} primitive {primitive_index} {kind:?} buffer requires {bytes} bytes, exceeding device limit {maximum_bytes}"
    )]
    BufferTooLarge {
        /// Logical asset identity.
        asset_id: bpp_core::AssetId,
        /// Stable source primitive index.
        primitive_index: usize,
        /// Buffer role.
        kind: GpuBufferKind,
        /// Required bytes.
        bytes: u64,
        /// Device maximum buffer size.
        maximum_bytes: u64,
    },
    /// The v1 wgpu material path cannot yet sample a base-color texture.
    #[error(
        "asset {asset_id:?} primitive {primitive_index} uses an unsupported base-color texture"
    )]
    UnsupportedBaseColorTexture {
        /// Logical asset identity.
        asset_id: bpp_core::AssetId,
        /// Stable source primitive index.
        primitive_index: usize,
    },
}

/// Failure to initialize the headless `wgpu` context.
#[derive(Debug, Error)]
pub enum WgpuInitializationError {
    /// No enumerated adapter satisfied the configured hard device-class filter.
    #[error(
        "no wgpu adapter matched device preference {device_preference:?} and power preference {adapter_preference:?}"
    )]
    NoMatchingAdapter {
        /// Required device class.
        device_preference: WgpuDevicePreference,
        /// Requested power ordering.
        adapter_preference: WgpuAdapterPreference,
    },
    /// The selected adapter rejected baseline device creation.
    #[error("wgpu device request failed: {0}")]
    RequestDevice(#[source] wgpu::RequestDeviceError),
    /// Fixed renderer provenance unexpectedly violated the core DTO contract.
    #[error("wgpu adapter provenance was invalid: {0}")]
    InvalidProvenance(#[from] InvalidBackendInfoError),
    /// A compile-time fixed renderer invariant was violated.
    #[error("internal wgpu renderer capability invariant failed")]
    InternalInvariant,
    /// Required canonical target format is not renderable on this adapter.
    #[error("wgpu adapter cannot render required target format {format:?}")]
    UnsupportedRenderFormat {
        /// Unsupported canonical format.
        format: wgpu::TextureFormat,
    },
    /// Configured shadow-map dimensions exceed the selected adapter limit.
    #[error("shadow map size {requested} exceeds adapter limit {maximum}")]
    ShadowMapTooLarge {
        /// Requested square shadow-map edge length.
        requested: u32,
        /// Maximum supported 2D texture edge length.
        maximum: u32,
    },
}

//! Headless wgpu renderer implementation.

use std::collections::BTreeMap;

use bpp_assets::{ResolvedAssetCatalog, VisualPrimitive};
use bpp_core::{
    BackendInfo, BackendKind, InvalidBackendInfoError, RenderChannels, RendererCapabilities,
    WgpuAdapterPreference, WgpuDevicePreference, WgpuRendererConfig,
};
use thiserror::Error;
use wgpu::util::DeviceExt as _;

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuVertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
}

/// Single-owner headless `wgpu` device context.
pub struct WgpuRenderer {
    config: WgpuRendererConfig,
    info: BackendInfo,
    capabilities: RendererCapabilities,
    _instance: wgpu::Instance,
    _adapter: wgpu::Adapter,
    device: wgpu::Device,
    _queue: wgpu::Queue,
    prepared: Option<PreparedGpuCatalog>,
}

struct PreparedGpuCatalog {
    fingerprint: String,
    _assets: BTreeMap<bpp_core::AssetId, GpuAsset>,
    stats: PreparedAssetStats,
}

struct GpuAsset {
    _primitives: Vec<GpuPrimitive>,
}

struct GpuPrimitive {
    _vertex_buffer: wgpu::Buffer,
    _index_buffer: wgpu::Buffer,
    _index_count: u32,
}

impl WgpuRenderer {
    /// Selects a matching adapter deterministically and requests one device.
    ///
    /// Explicit CPU/GPU preferences are hard filters. Within the accepted
    /// class, adapter type and stable adapter metadata break ties.
    ///
    /// # Errors
    ///
    /// Returns a typed error when no matching adapter exists or device
    /// creation fails.
    pub fn try_new(config: WgpuRendererConfig) -> Result<Self, WgpuInitializationError> {
        let instance = wgpu::Instance::default();
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
        let (adapter, adapter_info) = adapters
            .into_iter()
            .filter_map(|adapter| {
                let info = adapter.get_info();
                let rank = adapter_rank(
                    config.device_preference(),
                    config.adapter_preference(),
                    info.device_type,
                )?;
                let key = (
                    rank,
                    info.vendor,
                    info.device,
                    info.name.clone(),
                    format!("{:?}", info.backend),
                );
                Some((key, adapter, info))
            })
            .min_by(|left, right| left.0.cmp(&right.0))
            .map(|(_, adapter, info)| (adapter, info))
            .ok_or(WgpuInitializationError::NoMatchingAdapter {
                device_preference: config.device_preference(),
                adapter_preference: config.adapter_preference(),
            })?;

        let descriptor = wgpu::DeviceDescriptor {
            label: Some("bpp-render-wgpu"),
            ..Default::default()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&descriptor))
            .map_err(WgpuInitializationError::RequestDevice)?;
        let info = backend_info(&adapter_info, &adapter.limits())?;
        let channels = RenderChannels::try_new(true, true, true)
            .map_err(|_| WgpuInitializationError::InternalInvariant)?;
        let capabilities = RendererCapabilities::new(channels, None);
        Ok(Self {
            config,
            info,
            capabilities,
            _instance: instance,
            _adapter: adapter,
            device,
            _queue: queue,
            prepared: None,
        })
    }

    /// Returns checked renderer configuration used for initialization.
    #[must_use]
    pub const fn config(&self) -> WgpuRendererConfig {
        self.config
    }

    /// Returns stable adapter and implementation provenance.
    #[must_use]
    pub fn info(&self) -> BackendInfo {
        self.info.clone()
    }

    /// Returns the output contract intended for the headless render passes.
    #[must_use]
    pub const fn capabilities(&self) -> RendererCapabilities {
        self.capabilities
    }

    /// Uploads checked visual geometry once for the complete resolved catalog.
    ///
    /// Preparation is transactional: a failure retains the previously prepared
    /// catalog, if any, and exposes no partial replacement.
    ///
    /// # Errors
    ///
    /// Returns a typed overflow or device-limit error before creating an
    /// invalid GPU buffer.
    pub fn prepare(
        &mut self,
        catalog: &ResolvedAssetCatalog,
    ) -> Result<PreparedAssetStats, WgpuPreparationError> {
        let maximum_buffer_size = self.device.limits().max_buffer_size;
        let mut assets = BTreeMap::new();
        let mut stats = PreparedAssetStats {
            assets: u64::try_from(catalog.len())
                .map_err(|_| WgpuPreparationError::CountOverflow)?,
            primitives: 0,
            vertices: 0,
            indices: 0,
        };
        for entry in catalog.iter() {
            let mut primitives = Vec::new();
            for (primitive_index, primitive) in
                entry.asset().visual_mesh().primitives().iter().enumerate()
            {
                let vertices = pack_vertices(primitive);
                let vertex_bytes = bytemuck::cast_slice(&vertices);
                let index_bytes = bytemuck::cast_slice(primitive.indices());
                validate_buffer_size(
                    entry.asset_id(),
                    primitive_index,
                    GpuBufferKind::Vertex,
                    vertex_bytes.len(),
                    maximum_buffer_size,
                )?;
                validate_buffer_size(
                    entry.asset_id(),
                    primitive_index,
                    GpuBufferKind::Index,
                    index_bytes.len(),
                    maximum_buffer_size,
                )?;
                let index_count = u32::try_from(primitive.indices().len())
                    .map_err(|_| WgpuPreparationError::CountOverflow)?;
                stats.primitives = stats
                    .primitives
                    .checked_add(1)
                    .ok_or(WgpuPreparationError::CountOverflow)?;
                stats.vertices = stats
                    .vertices
                    .checked_add(
                        u64::try_from(vertices.len())
                            .map_err(|_| WgpuPreparationError::CountOverflow)?,
                    )
                    .ok_or(WgpuPreparationError::CountOverflow)?;
                stats.indices = stats
                    .indices
                    .checked_add(u64::from(index_count))
                    .ok_or(WgpuPreparationError::CountOverflow)?;
                let vertex_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("bpp asset vertices"),
                            contents: vertex_bytes,
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                let index_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("bpp asset indices"),
                            contents: index_bytes,
                            usage: wgpu::BufferUsages::INDEX,
                        });
                primitives.push(GpuPrimitive {
                    _vertex_buffer: vertex_buffer,
                    _index_buffer: index_buffer,
                    _index_count: index_count,
                });
            }
            assets.insert(
                entry.asset_id().clone(),
                GpuAsset {
                    _primitives: primitives,
                },
            );
        }
        self.prepared = Some(PreparedGpuCatalog {
            fingerprint: catalog.fingerprint().to_owned(),
            _assets: assets,
            stats,
        });
        Ok(stats)
    }

    /// Returns the prepared catalog identity, if preparation succeeded.
    #[must_use]
    pub fn prepared_catalog_fingerprint(&self) -> Option<&str> {
        self.prepared
            .as_ref()
            .map(|prepared| prepared.fingerprint.as_str())
    }

    /// Returns upload counts for the active prepared catalog.
    #[must_use]
    pub fn prepared_asset_stats(&self) -> Option<PreparedAssetStats> {
        self.prepared.as_ref().map(|prepared| prepared.stats)
    }
}

/// Exact geometry counts uploaded for one resolved asset catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreparedAssetStats {
    assets: u64,
    primitives: u64,
    vertices: u64,
    indices: u64,
}

impl PreparedAssetStats {
    /// Returns the number of assets.
    #[must_use]
    pub const fn asset_count(self) -> u64 {
        self.assets
    }
    /// Returns the number of source-order visual primitives.
    #[must_use]
    pub const fn primitive_count(self) -> u64 {
        self.primitives
    }
    /// Returns the total uploaded vertex count.
    #[must_use]
    pub const fn vertex_count(self) -> u64 {
        self.vertices
    }
    /// Returns the total uploaded index count.
    #[must_use]
    pub const fn index_count(self) -> u64 {
        self.indices
    }
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
}

fn adapter_rank(
    device_preference: WgpuDevicePreference,
    adapter_preference: WgpuAdapterPreference,
    device_type: wgpu::DeviceType,
) -> Option<u8> {
    let accepted = match device_preference {
        WgpuDevicePreference::Automatic => true,
        WgpuDevicePreference::Cpu => device_type == wgpu::DeviceType::Cpu,
        WgpuDevicePreference::Gpu => device_type != wgpu::DeviceType::Cpu,
    };
    if !accepted {
        return None;
    }
    let rank = match adapter_preference {
        WgpuAdapterPreference::LowPower => match device_type {
            wgpu::DeviceType::IntegratedGpu => 0,
            wgpu::DeviceType::DiscreteGpu => 1,
            wgpu::DeviceType::VirtualGpu => 2,
            wgpu::DeviceType::Cpu => 3,
            wgpu::DeviceType::Other => 4,
        },
        WgpuAdapterPreference::Automatic | WgpuAdapterPreference::HighPerformance => {
            match device_type {
                wgpu::DeviceType::DiscreteGpu => 0,
                wgpu::DeviceType::IntegratedGpu => 1,
                wgpu::DeviceType::VirtualGpu => 2,
                wgpu::DeviceType::Cpu => 3,
                wgpu::DeviceType::Other => 4,
            }
        }
    };
    Some(rank)
}

fn pack_vertices(primitive: &VisualPrimitive) -> Vec<GpuVertex> {
    let texture_coordinates = primitive.tex_coord_sets().first();
    primitive
        .positions()
        .iter()
        .copied()
        .zip(primitive.normals().iter().copied())
        .enumerate()
        .map(|(index, (position, normal))| GpuVertex {
            position,
            normal,
            uv: texture_coordinates.map_or([0.0, 0.0], |set| set[index]),
        })
        .collect()
}

fn validate_buffer_size(
    asset_id: &bpp_core::AssetId,
    primitive_index: usize,
    kind: GpuBufferKind,
    bytes: usize,
    maximum_bytes: u64,
) -> Result<(), WgpuPreparationError> {
    let bytes = u64::try_from(bytes).map_err(|_| WgpuPreparationError::CountOverflow)?;
    if bytes > maximum_bytes {
        return Err(WgpuPreparationError::BufferTooLarge {
            asset_id: asset_id.clone(),
            primitive_index,
            kind,
            bytes,
            maximum_bytes,
        });
    }
    Ok(())
}

fn backend_info(
    adapter: &wgpu::AdapterInfo,
    limits: &wgpu::Limits,
) -> Result<BackendInfo, InvalidBackendInfoError> {
    let mut properties = BTreeMap::from([
        ("backend".to_owned(), format!("{:?}", adapter.backend)),
        ("device_id".to_owned(), adapter.device.to_string()),
        (
            "device_type".to_owned(),
            format!("{:?}", adapter.device_type),
        ),
        (
            "max_texture_dimension_2d".to_owned(),
            limits.max_texture_dimension_2d.to_string(),
        ),
        ("vendor_id".to_owned(), adapter.vendor.to_string()),
    ]);
    if !adapter.driver.trim().is_empty() {
        properties.insert("driver".to_owned(), adapter.driver.clone());
    }
    if !adapter.driver_info.trim().is_empty() {
        properties.insert("driver_info".to_owned(), adapter.driver_info.clone());
    }
    BackendInfo::try_new(
        BackendKind::Wgpu,
        env!("CARGO_PKG_VERSION"),
        (!adapter.name.trim().is_empty()).then(|| adapter.name.clone()),
        properties,
    )
}

#[cfg(test)]
mod tests {
    use bpp_assets::VisualPrimitive;
    use bpp_core::{WgpuAdapterPreference, WgpuDevicePreference};

    use super::{adapter_rank, pack_vertices};

    fn assert_float_array<const N: usize>(actual: [f32; N], expected: [f32; N]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
    }

    #[test]
    fn explicit_device_class_is_a_hard_filter() {
        assert!(
            adapter_rank(
                WgpuDevicePreference::Cpu,
                WgpuAdapterPreference::Automatic,
                wgpu::DeviceType::Cpu,
            )
            .is_some()
        );
        assert_eq!(
            adapter_rank(
                WgpuDevicePreference::Cpu,
                WgpuAdapterPreference::Automatic,
                wgpu::DeviceType::DiscreteGpu,
            ),
            None
        );
        assert_eq!(
            adapter_rank(
                WgpuDevicePreference::Gpu,
                WgpuAdapterPreference::Automatic,
                wgpu::DeviceType::Cpu,
            ),
            None
        );
    }

    #[test]
    fn power_preference_has_a_deterministic_device_type_order() {
        let high_discrete = adapter_rank(
            WgpuDevicePreference::Automatic,
            WgpuAdapterPreference::HighPerformance,
            wgpu::DeviceType::DiscreteGpu,
        )
        .unwrap();
        let high_integrated = adapter_rank(
            WgpuDevicePreference::Automatic,
            WgpuAdapterPreference::HighPerformance,
            wgpu::DeviceType::IntegratedGpu,
        )
        .unwrap();
        let low_discrete = adapter_rank(
            WgpuDevicePreference::Automatic,
            WgpuAdapterPreference::LowPower,
            wgpu::DeviceType::DiscreteGpu,
        )
        .unwrap();
        let low_integrated = adapter_rank(
            WgpuDevicePreference::Automatic,
            WgpuAdapterPreference::LowPower,
            wgpu::DeviceType::IntegratedGpu,
        )
        .unwrap();

        assert!(high_discrete < high_integrated);
        assert!(low_integrated < low_discrete);
    }

    #[test]
    fn gpu_vertex_packing_uses_first_uv_set_or_zero_without_losing_normals() {
        let without_uv = VisualPrimitive::try_new(
            vec![[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 9.0]],
            vec![[0.0, 0.0, 1.0]; 3],
            vec![],
            vec![0, 1, 2],
            None,
        )
        .unwrap();
        let packed = pack_vertices(&without_uv);
        assert_float_array(packed[0].position, [1.0, 2.0, 3.0]);
        assert_float_array(packed[0].normal, [0.0, 0.0, 1.0]);
        assert_float_array(packed[0].uv, [0.0, 0.0]);

        let with_uv = VisualPrimitive::try_new(
            without_uv.positions().to_vec(),
            without_uv.normals().to_vec(),
            vec![vec![[0.25, 0.75]; 3], vec![[0.5, 0.5]; 3]],
            without_uv.indices().to_vec(),
            None,
        )
        .unwrap();
        assert_float_array(pack_vertices(&with_uv)[0].uv, [0.25, 0.75]);
    }
}

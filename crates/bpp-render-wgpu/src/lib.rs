//! Headless wgpu renderer implementation.

use std::collections::{BTreeMap, VecDeque};
use std::{
    num::NonZeroU32,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use bpp_assets::{ResolvedAssetCatalog, VisualPrimitive};
use bpp_core::{
    AppearanceSnapshot, BackendInfo, BackendKind, CameraSnapshot, InvalidBackendInfoError,
    NonNegativeFinite, RenderChannels, RenderError, RenderReport, RenderSceneRequest,
    RenderStageTimings, RendererCapabilities, RigidTransform, SceneSnapshot, WgpuAdapterPreference,
    WgpuDevicePreference, WgpuRendererConfig,
};
use bpp_dataset::{DepthFrame, InstanceFrame, RgbFrame, SceneStagingArea};
use bpp_render_api::Renderer;
use thiserror::Error;
use wgpu::util::DeviceExt as _;

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuVertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    camera_from_world: [[f32; 4]; 4],
    projection: [f32; 4],
    image_and_clip: [f32; 4],
    far_and_padding: [f32; 4],
    camera_world_position: [f32; 4],
    ambient: [f32; 4],
    light_counts: [u32; 4],
    directional_directions: [[f32; 4]; MAX_LIGHTS],
    directional_colors: [[f32; 4]; MAX_LIGHTS],
    point_positions: [[f32; 4]; MAX_LIGHTS],
    point_colors: [[f32; 4]; MAX_LIGHTS],
    light_clip_from_world: [[f32; 4]; 4],
    shadow: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawUniform {
    world_from_model: [[f32; 4]; 4],
    normal_from_model: [[f32; 4]; 4],
    base_color: [f32; 4],
    material: [f32; 4],
    instance_id: u32,
    padding: [u32; 3],
}

const MAX_LIGHTS: usize = 8;
const GPU_VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 3] =
    wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2];

/// Single-owner headless `wgpu` device context.
pub struct WgpuRenderer {
    config: WgpuRendererConfig,
    info: BackendInfo,
    capabilities: RendererCapabilities,
    _instance: wgpu::Instance,
    _adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    render_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    camera_layout: wgpu::BindGroupLayout,
    draws_layout: wgpu::BindGroupLayout,
    shadow_layout: wgpu::BindGroupLayout,
    fxaa_pipeline: wgpu::RenderPipeline,
    fxaa_layout: wgpu::BindGroupLayout,
    fxaa_sampler: wgpu::Sampler,
    timestamp_queries: bool,
    timestamp_period_ns: f32,
    device_loss: Arc<Mutex<Option<String>>>,
    prepared: Option<PreparedGpuCatalog>,
}

struct PreparedGpuCatalog {
    fingerprint: String,
    assets: BTreeMap<bpp_core::AssetId, GpuAsset>,
    stats: PreparedAssetStats,
}

struct GpuAsset {
    primitives: Vec<GpuPrimitive>,
}

struct GpuPrimitive {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    base_color: [f32; 4],
    metallic: f32,
    roughness: f32,
    bounds_min: [f32; 3],
    bounds_max: [f32; 3],
}

struct ReadbackFrame {
    rgba8: Vec<u8>,
    depth: Vec<f32>,
    instance: Vec<u16>,
    gpu_seconds: Option<f32>,
}

struct PendingReadback {
    submission: wgpu::SubmissionIndex,
    rgb: ReadbackBuffer,
    depth: ReadbackBuffer,
    instance: ReadbackBuffer,
    rgb_mapping: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    depth_mapping: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    instance_mapping: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    timestamp: Option<PendingTimestamp>,
}

struct PendingTimestamp {
    buffer: wgpu::Buffer,
    mapping: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    period_ns: f32,
}

const READBACK_RING_CAPACITY: usize = 3;

struct DrawCall<'a> {
    primitive: &'a GpuPrimitive,
    draw_index: u32,
}

struct TargetViews<'a> {
    rgb: &'a wgpu::TextureView,
    depth: &'a wgpu::TextureView,
    instance: &'a wgpu::TextureView,
    depth_test: &'a wgpu::TextureView,
}

struct SceneBindings {
    _camera_buffer: wgpu::Buffer,
    _draws_buffer: wgpu::Buffer,
    camera: wgpu::BindGroup,
    draws: wgpu::BindGroup,
}

struct ShadowResources {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

struct TimestampResources {
    query_set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
}

struct ScenePassInputs<'a> {
    targets: TargetViews<'a>,
    clear: [f32; 4],
    bindings: Option<&'a SceneBindings>,
    draw_calls: &'a [DrawCall<'a>],
    shadow: Option<&'a ShadowResources>,
    timestamp_query_set: Option<&'a wgpu::QuerySet>,
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

        let timestamp_queries = adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        let descriptor = wgpu::DeviceDescriptor {
            label: Some("bpp-render-wgpu"),
            required_features: if timestamp_queries {
                wgpu::Features::TIMESTAMP_QUERY
            } else {
                wgpu::Features::empty()
            },
            ..Default::default()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&descriptor))
            .map_err(WgpuInitializationError::RequestDevice)?;
        let device_loss = Arc::new(Mutex::new(None));
        let callback_state = Arc::clone(&device_loss);
        device.set_device_lost_callback(move |reason, message| {
            record_device_loss(&callback_state, format!("{reason:?}: {message}"));
        });
        for format in [
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::R32Float,
            wgpu::TextureFormat::R32Uint,
            wgpu::TextureFormat::Depth32Float,
        ] {
            let features = adapter.get_texture_format_features(format);
            if !features
                .allowed_usages
                .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
            {
                return Err(WgpuInitializationError::UnsupportedRenderFormat { format });
            }
        }
        let maximum_shadow_map_size = adapter.limits().max_texture_dimension_2d;
        if config.shadow_map_size() > maximum_shadow_map_size {
            return Err(WgpuInitializationError::ShadowMapTooLarge {
                requested: config.shadow_map_size(),
                maximum: maximum_shadow_map_size,
            });
        }
        let (render_pipeline, shadow_pipeline, camera_layout, draws_layout, shadow_layout) =
            create_scene_pipeline(&device);
        let (fxaa_pipeline, fxaa_layout) = create_fxaa_pipeline(&device);
        let fxaa_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bpp FXAA sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let info = backend_info(&adapter_info, &adapter.limits(), timestamp_queries)?;
        let channels = RenderChannels::try_new(true, true, true)
            .map_err(|_| WgpuInitializationError::InternalInvariant)?;
        let capabilities = RendererCapabilities::new(channels, None);
        let timestamp_period_ns = queue.get_timestamp_period();
        Ok(Self {
            config,
            info,
            capabilities,
            _instance: instance,
            _adapter: adapter,
            device,
            queue,
            render_pipeline,
            shadow_pipeline,
            camera_layout,
            draws_layout,
            shadow_layout,
            fxaa_pipeline,
            fxaa_layout,
            fxaa_sampler,
            timestamp_queries,
            timestamp_period_ns,
            device_loss,
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
                let material = primitive
                    .material()
                    .and_then(|index| usize::try_from(index.get()).ok())
                    .and_then(|index| entry.asset().materials().get(index));
                if material.is_some_and(|material| material.base_color_texture().is_some()) {
                    return Err(WgpuPreparationError::UnsupportedBaseColorTexture {
                        asset_id: entry.asset_id().clone(),
                        primitive_index,
                    });
                }
                let base_color =
                    material.map_or([1.0; 4], bpp_assets::PbrMaterial::base_color_factor);
                let metallic = material.map_or(0.0, bpp_assets::PbrMaterial::metallic_factor);
                let roughness = material.map_or(1.0, bpp_assets::PbrMaterial::roughness_factor);
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
                let (bounds_min, bounds_max) = primitive_bounds(primitive);
                primitives.push(GpuPrimitive {
                    vertex_buffer,
                    index_buffer,
                    index_count,
                    base_color,
                    metallic,
                    roughness,
                    bounds_min,
                    bounds_max,
                });
            }
            assets.insert(entry.asset_id().clone(), GpuAsset { primitives });
        }
        self.prepared = Some(PreparedGpuCatalog {
            fingerprint: catalog.fingerprint().to_owned(),
            assets,
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

    #[cfg(test)]
    fn render_clear_targets(
        &self,
        width: u32,
        height: u32,
        background_srgb: [f32; 4],
    ) -> Result<ReadbackFrame, WgpuReadbackError> {
        self.render_targets(width, height, background_srgb, None, &[], &[])
    }

    #[cfg(test)]
    fn render_targets(
        &self,
        width: u32,
        height: u32,
        background_srgb: [f32; 4],
        camera: Option<&CameraUniform>,
        draw_uniforms: &[DrawUniform],
        draw_calls: &[DrawCall<'_>],
    ) -> Result<ReadbackFrame, WgpuReadbackError> {
        self.begin_render_targets(
            width,
            height,
            background_srgb,
            camera,
            draw_uniforms,
            draw_calls,
        )?
        .finish(&self.device)
    }

    fn begin_render_targets(
        &self,
        width: u32,
        height: u32,
        background_srgb: [f32; 4],
        camera: Option<&CameraUniform>,
        draw_uniforms: &[DrawUniform],
        draw_calls: &[DrawCall<'_>],
    ) -> Result<PendingReadback, WgpuReadbackError> {
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let rgb = create_target(
            &self.device,
            extent,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            "rgb",
        );
        let depth = create_target(&self.device, extent, wgpu::TextureFormat::R32Float, "depth");
        let instance = create_target(
            &self.device,
            extent,
            wgpu::TextureFormat::R32Uint,
            "instance",
        );
        let rgb_view = rgb.create_view(&wgpu::TextureViewDescriptor::default());
        let fxaa_source = self.config.rgb_fxaa().then(|| {
            create_sampled_target(
                &self.device,
                extent,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                "FXAA source",
            )
        });
        let fxaa_source_view = fxaa_source
            .as_ref()
            .map(|texture| texture.create_view(&wgpu::TextureViewDescriptor::default()));
        let scene_rgb_view = fxaa_source_view.as_ref().unwrap_or(&rgb_view);
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        let instance_view = instance.create_view(&wgpu::TextureViewDescriptor::default());
        let depth_test = create_target(
            &self.device,
            extent,
            wgpu::TextureFormat::Depth32Float,
            "depth test",
        );
        let depth_test_view = depth_test.create_view(&wgpu::TextureViewDescriptor::default());
        let rgb_readback = ReadbackBuffer::new(&self.device, width, height, 4, "rgb")?;
        let depth_readback = ReadbackBuffer::new(&self.device, width, height, 4, "depth")?;
        let instance_readback = ReadbackBuffer::new(&self.device, width, height, 4, "instance")?;
        let linear = background_srgb.map(srgb_to_linear);
        let bindings = camera
            .zip((!draw_uniforms.is_empty()).then_some(draw_uniforms))
            .map(|(camera, draws)| self.create_scene_bindings(camera, draws));
        let shadow = bindings.as_ref().map(|_| self.create_shadow_resources());
        let timestamp = self.create_timestamp_resources();

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("bpp clear targets"),
            });
        self.encode_scene_pass(
            &mut encoder,
            &ScenePassInputs {
                targets: TargetViews {
                    rgb: scene_rgb_view,
                    depth: &depth_view,
                    instance: &instance_view,
                    depth_test: &depth_test_view,
                },
                clear: linear,
                bindings: bindings.as_ref(),
                draw_calls,
                shadow: shadow.as_ref(),
                timestamp_query_set: timestamp.as_ref().map(|resources| &resources.query_set),
            },
        );
        if let Some(source_view) = &fxaa_source_view {
            self.encode_fxaa_pass(&mut encoder, source_view, &rgb_view, width, height);
        }
        if let Some(timestamp) = &timestamp {
            encoder.resolve_query_set(&timestamp.query_set, 0..2, &timestamp.resolve, 0);
            encoder.copy_buffer_to_buffer(&timestamp.resolve, 0, &timestamp.readback, 0, 16);
        }
        rgb_readback.copy_from(&mut encoder, &rgb, extent);
        depth_readback.copy_from(&mut encoder, &depth, extent);
        instance_readback.copy_from(&mut encoder, &instance, extent);
        let submission = self.queue.submit([encoder.finish()]);
        let rgb_mapping = rgb_readback.begin_read();
        let depth_mapping = depth_readback.begin_read();
        let instance_mapping = instance_readback.begin_read();
        let pending_timestamp = timestamp.map(|timestamp| self.begin_timestamp_read(timestamp));
        Ok(PendingReadback {
            submission,
            rgb: rgb_readback,
            depth: depth_readback,
            instance: instance_readback,
            rgb_mapping,
            depth_mapping,
            instance_mapping,
            timestamp: pending_timestamp,
        })
    }

    fn create_scene_bindings(
        &self,
        camera: &CameraUniform,
        draws: &[DrawUniform],
    ) -> SceneBindings {
        let camera_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bpp camera uniform"),
                contents: bytemuck::bytes_of(camera),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let draws_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bpp draw storage"),
                contents: bytemuck::cast_slice(draws),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let camera = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bpp camera bind group"),
            layout: &self.camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        let draws = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bpp draws bind group"),
            layout: &self.draws_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: draws_buffer.as_entire_binding(),
            }],
        });
        SceneBindings {
            _camera_buffer: camera_buffer,
            _draws_buffer: draws_buffer,
            camera,
            draws,
        }
    }

    fn create_timestamp_resources(&self) -> Option<TimestampResources> {
        self.timestamp_queries.then(|| TimestampResources {
            query_set: self.device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("bpp render timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: 2,
            }),
            resolve: self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bpp timestamp resolve"),
                size: 16,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            readback: self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bpp timestamp readback"),
                size: 16,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
        })
    }

    fn begin_timestamp_read(&self, timestamp: TimestampResources) -> PendingTimestamp {
        let slice = timestamp.readback.slice(..);
        let (sender, mapping) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        PendingTimestamp {
            buffer: timestamp.readback,
            mapping,
            period_ns: self.timestamp_period_ns,
        }
    }

    fn encode_scene_pass(&self, encoder: &mut wgpu::CommandEncoder, inputs: &ScenePassInputs<'_>) {
        if let (Some(bindings), Some(shadow)) = (inputs.bindings, inputs.shadow) {
            let mut shadow_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bpp directional shadow pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &shadow.view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            shadow_pass.set_pipeline(&self.shadow_pipeline);
            shadow_pass.set_bind_group(0, &bindings.camera, &[]);
            shadow_pass.set_bind_group(1, &bindings.draws, &[]);
            encode_draw_calls(&mut shadow_pass, inputs.draw_calls);
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("bpp scene pass"),
            color_attachments: &[
                Some(color_attachment(
                    inputs.targets.rgb,
                    wgpu::Color {
                        r: f64::from(inputs.clear[0]),
                        g: f64::from(inputs.clear[1]),
                        b: f64::from(inputs.clear[2]),
                        a: f64::from(inputs.clear[3]),
                    },
                )),
                Some(color_attachment(
                    inputs.targets.depth,
                    wgpu::Color::TRANSPARENT,
                )),
                Some(color_attachment(
                    inputs.targets.instance,
                    wgpu::Color::TRANSPARENT,
                )),
            ],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: inputs.targets.depth_test,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: inputs.timestamp_query_set.map(|query_set| {
                wgpu::RenderPassTimestampWrites {
                    query_set,
                    beginning_of_pass_write_index: Some(0),
                    end_of_pass_write_index: Some(1),
                }
            }),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(bindings) = inputs.bindings {
            pass.set_pipeline(&self.render_pipeline);
            pass.set_bind_group(0, &bindings.camera, &[]);
            pass.set_bind_group(1, &bindings.draws, &[]);
            pass.set_bind_group(
                2,
                &inputs
                    .shadow
                    .expect("bindings require shadow resources")
                    .bind_group,
                &[],
            );
            encode_draw_calls(&mut pass, inputs.draw_calls);
        }
    }

    fn create_shadow_resources(&self) -> ShadowResources {
        let size = self.config.shadow_map_size();
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bpp directional shadow map"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bpp shadow comparison sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bpp shadow bind group"),
            layout: &self.shadow_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        ShadowResources {
            _texture: texture,
            view,
            bind_group,
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn encode_fxaa_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::TextureView,
        target: &wgpu::TextureView,
        width: u32,
        height: u32,
    ) {
        let dimensions = [width as f32, height as f32, 0.0, 0.0];
        let dimensions_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bpp FXAA dimensions"),
                contents: bytemuck::cast_slice(&dimensions),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bpp FXAA bind group"),
            layout: &self.fxaa_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.fxaa_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: dimensions_buffer.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("bpp RGB FXAA pass"),
            color_attachments: &[Some(color_attachment(target, wgpu::Color::TRANSPARENT))],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.fxaa_pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    fn begin_camera(
        &self,
        camera: &CameraSnapshot,
        scene: &SceneSnapshot,
        appearance: &AppearanceSnapshot,
    ) -> Result<PendingReadback, WgpuSceneError> {
        self.ensure_device_alive()?;
        let prepared = self.prepared.as_ref().ok_or(WgpuSceneError::NotPrepared)?;
        let mut uniforms = Vec::new();
        let mut calls = Vec::new();
        for instance in scene.instances() {
            let asset = prepared.assets.get(instance.asset_id()).ok_or_else(|| {
                WgpuSceneError::MissingPreparedAsset {
                    asset_id: instance.asset_id().clone(),
                }
            })?;
            let world_from_model = rigid_matrix(instance.world_from_model());
            let rotation = rotation_matrix(instance.world_from_model());
            for primitive in &asset.primitives {
                let draw_index =
                    u32::try_from(uniforms.len()).map_err(|_| WgpuSceneError::DrawCountOverflow)?;
                uniforms.push(DrawUniform {
                    world_from_model: world_from_model.to_cols_array_2d(),
                    normal_from_model: rotation.to_cols_array_2d(),
                    base_color: primitive.base_color,
                    material: [primitive.metallic, primitive.roughness, 0.0, 0.0],
                    instance_id: u32::from(instance.instance_id().get()),
                    padding: [0; 3],
                });
                calls.push(DrawCall {
                    primitive,
                    draw_index,
                });
            }
        }
        let size = camera.image_size();
        if uniforms.is_empty() {
            return self
                .begin_render_targets(
                    size.width(),
                    size.height(),
                    appearance.background_srgb().rgba(),
                    None,
                    &[],
                    &[],
                )
                .map_err(WgpuSceneError::Readback);
        }
        let camera_from_world = rigid_matrix(camera.camera_from_world().camera_from_world());
        let (light_clip_from_world, shadow_enabled) =
            shadow_projection(appearance, &uniforms, &calls);
        let camera_uniform = camera_uniform(
            camera,
            appearance,
            camera_from_world,
            light_clip_from_world,
            shadow_enabled,
        )?;
        self.begin_render_targets(
            size.width(),
            size.height(),
            appearance.background_srgb().rgba(),
            Some(&camera_uniform),
            &uniforms,
            &calls,
        )
        .map_err(WgpuSceneError::Readback)
    }

    #[cfg(test)]
    fn render_camera(
        &self,
        camera: &CameraSnapshot,
        scene: &SceneSnapshot,
        appearance: &AppearanceSnapshot,
    ) -> Result<ReadbackFrame, WgpuSceneError> {
        self.begin_camera(camera, scene, appearance)?
            .finish(&self.device)
            .map_err(WgpuSceneError::Readback)
    }

    fn ensure_device_alive(&self) -> Result<(), WgpuSceneError> {
        let message = self
            .device_loss
            .lock()
            .map_err(|_| WgpuSceneError::DeviceLossStatePoisoned)?
            .clone();
        message.map_or(Ok(()), |message| {
            Err(WgpuSceneError::DeviceLost { message })
        })
    }
}

fn record_device_loss(state: &Mutex<Option<String>>, message: String) {
    if let Ok(mut recorded) = state.lock() {
        *recorded = Some(message);
    }
}

impl PendingReadback {
    fn finish(self, device: &wgpu::Device) -> Result<ReadbackFrame, WgpuReadbackError> {
        device.poll(wgpu::PollType::Wait {
            submission_index: Some(self.submission),
            timeout: Some(Duration::from_secs(30)),
        })?;
        let rgba8 = self.rgb.finish_read(&self.rgb_mapping)?;
        let depth_bytes = self.depth.finish_read(&self.depth_mapping)?;
        let instance_bytes = self.instance.finish_read(&self.instance_mapping)?;
        let depth = depth_bytes
            .chunks_exact(4)
            .map(|bytes| f32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect();
        let instance = instance_bytes
            .chunks_exact(4)
            .map(|bytes| u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .map(|value| {
                u16::try_from(value).map_err(|_| WgpuReadbackError::InstanceIdOverflow { value })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let gpu_seconds = self.timestamp.map(PendingTimestamp::finish).transpose()?;
        Ok(ReadbackFrame {
            rgba8,
            depth,
            instance,
            gpu_seconds,
        })
    }
}

impl PendingTimestamp {
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    fn finish(self) -> Result<f32, WgpuReadbackError> {
        receive_mapping(&self.mapping).map_err(map_mapping_completion)?;
        let mapped = self.buffer.slice(..).get_mapped_range()?;
        let timestamps: &[u64] = bytemuck::cast_slice(&mapped);
        let ticks = timestamps[1].saturating_sub(timestamps[0]);
        let seconds = (ticks as f64 * f64::from(self.period_ns) * 1.0e-9) as f32;
        drop(mapped);
        self.buffer.unmap();
        if seconds.is_finite() {
            Ok(seconds)
        } else {
            Err(WgpuReadbackError::TimestampOverflow)
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum MappingCompletionError<E> {
    CallbackDropped,
    Mapping(E),
}

fn receive_mapping<E>(
    receiver: &mpsc::Receiver<Result<(), E>>,
) -> Result<(), MappingCompletionError<E>> {
    receiver
        .recv()
        .map_err(|_| MappingCompletionError::CallbackDropped)?
        .map_err(MappingCompletionError::Mapping)
}

fn map_mapping_completion(
    error: MappingCompletionError<wgpu::BufferAsyncError>,
) -> WgpuReadbackError {
    match error {
        MappingCompletionError::CallbackDropped => WgpuReadbackError::CallbackDropped,
        MappingCompletionError::Mapping(error) => WgpuReadbackError::Map(error),
    }
}

fn camera_uniform(
    camera: &CameraSnapshot,
    appearance: &AppearanceSnapshot,
    camera_from_world: glam::Mat4,
    light_clip_from_world: glam::Mat4,
    shadow_enabled: bool,
) -> Result<CameraUniform, WgpuSceneError> {
    if appearance.directional_lights().len() > MAX_LIGHTS
        || appearance.point_lights().len() > MAX_LIGHTS
    {
        return Err(WgpuSceneError::TooManyLights {
            maximum: MAX_LIGHTS,
        });
    }
    let size = camera.image_size();
    let intrinsics = camera.intrinsics();
    let mut directional_directions = [[0.0; 4]; MAX_LIGHTS];
    let mut directional_colors = [[0.0; 4]; MAX_LIGHTS];
    for (index, light) in appearance.directional_lights().iter().copied().enumerate() {
        let direction = light.direction_world();
        directional_directions[index] = [direction[0], direction[1], direction[2], 0.0];
        let color = light.color().rgb();
        let scale = light.illuminance_lux().get() / 1000.0;
        directional_colors[index] = [color[0] * scale, color[1] * scale, color[2] * scale, 0.0];
    }
    let mut point_positions = [[0.0; 4]; MAX_LIGHTS];
    let mut point_colors = [[0.0; 4]; MAX_LIGHTS];
    for (index, light) in appearance.point_lights().iter().copied().enumerate() {
        let position = light.position_world_m().map(bpp_core::Meters::get);
        point_positions[index] = [position[0], position[1], position[2], 0.0];
        let color = light.color().rgb();
        let scale = light.luminous_power_lm().get() / (4.0 * std::f32::consts::PI);
        point_colors[index] = [color[0] * scale, color[1] * scale, color[2] * scale, 0.0];
    }
    let ambient = appearance.ambient_light();
    let ambient_color = ambient.color().rgb();
    let ambient_scale = ambient.intensity().get();
    let camera_position = camera_from_world
        .inverse()
        .transform_point3(glam::Vec3::ZERO)
        .to_array();
    Ok(CameraUniform {
        camera_from_world: camera_from_world.to_cols_array_2d(),
        projection: [
            intrinsics.fx(),
            intrinsics.skew(),
            intrinsics.cx(),
            intrinsics.fy(),
        ],
        image_and_clip: [
            intrinsics.cy(),
            dimension_as_f32(size.width())?,
            dimension_as_f32(size.height())?,
            camera.near_m().get(),
        ],
        far_and_padding: [camera.far_m().get(), 0.0, 0.0, 0.0],
        camera_world_position: [
            camera_position[0],
            camera_position[1],
            camera_position[2],
            0.0,
        ],
        ambient: [
            ambient_color[0] * ambient_scale,
            ambient_color[1] * ambient_scale,
            ambient_color[2] * ambient_scale,
            0.0,
        ],
        light_counts: [
            u32::try_from(appearance.directional_lights().len()).expect("bounded"),
            u32::try_from(appearance.point_lights().len()).expect("bounded"),
            0,
            0,
        ],
        directional_directions,
        directional_colors,
        point_positions,
        point_colors,
        light_clip_from_world: light_clip_from_world.to_cols_array_2d(),
        shadow: [f32::from(shadow_enabled), 0.001_5, 0.0, 0.0],
    })
}

fn primitive_bounds(primitive: &VisualPrimitive) -> ([f32; 3], [f32; 3]) {
    let mut minimum = glam::Vec3::splat(f32::INFINITY);
    let mut maximum = glam::Vec3::splat(f32::NEG_INFINITY);
    for position in primitive.positions() {
        let point = glam::Vec3::from_array(*position);
        minimum = minimum.min(point);
        maximum = maximum.max(point);
    }
    (minimum.to_array(), maximum.to_array())
}

fn shadow_projection(
    appearance: &AppearanceSnapshot,
    uniforms: &[DrawUniform],
    calls: &[DrawCall<'_>],
) -> (glam::Mat4, bool) {
    let Some(light) = appearance.directional_lights().first() else {
        return (glam::Mat4::IDENTITY, false);
    };
    let mut minimum = glam::Vec3::splat(f32::INFINITY);
    let mut maximum = glam::Vec3::splat(f32::NEG_INFINITY);
    for (uniform, call) in uniforms.iter().zip(calls) {
        let world_from_model = glam::Mat4::from_cols_array_2d(&uniform.world_from_model);
        for x in [call.primitive.bounds_min[0], call.primitive.bounds_max[0]] {
            for y in [call.primitive.bounds_min[1], call.primitive.bounds_max[1]] {
                for z in [call.primitive.bounds_min[2], call.primitive.bounds_max[2]] {
                    let point = world_from_model.transform_point3(glam::vec3(x, y, z));
                    minimum = minimum.min(point);
                    maximum = maximum.max(point);
                }
            }
        }
    }
    if !minimum.is_finite() || !maximum.is_finite() {
        return (glam::Mat4::IDENTITY, false);
    }
    let direction = glam::Vec3::from_array(light.direction_world()).normalize();
    (directional_shadow_matrix(direction, minimum, maximum), true)
}

fn directional_shadow_matrix(
    direction: glam::Vec3,
    minimum: glam::Vec3,
    maximum: glam::Vec3,
) -> glam::Mat4 {
    let center = (minimum + maximum) * 0.5;
    let radius = ((maximum - minimum).length() * 0.5).max(0.01);
    let up = if direction.z.abs() > 0.95 {
        glam::Vec3::X
    } else {
        glam::Vec3::Z
    };
    let eye = center - direction * (radius * 2.0 + 0.1);
    let view = glam::Mat4::look_at_rh(eye, center, up);
    let projection =
        glam::Mat4::orthographic_rh(-radius, radius, -radius, radius, 0.01, radius * 4.0 + 0.2);
    projection * view
}

#[derive(Debug, Error)]
enum WgpuSceneError {
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

impl Renderer for WgpuRenderer {
    fn info(&self) -> BackendInfo {
        WgpuRenderer::info(self)
    }

    fn capabilities(&self) -> RendererCapabilities {
        WgpuRenderer::capabilities(self)
    }

    fn prepare(&mut self, assets: &ResolvedAssetCatalog) -> Result<(), RenderError> {
        WgpuRenderer::prepare(self, assets)
            .map(|_| ())
            .map_err(|error| RenderError::InvalidContent {
                message: error.to_string(),
            })
    }

    fn render_scene(
        &mut self,
        request: &RenderSceneRequest,
        staging: &SceneStagingArea,
    ) -> Result<RenderReport, RenderError> {
        if self.prepared.is_none() {
            return Err(RenderError::NotPrepared);
        }
        let camera_count = u32::try_from(request.cameras().len()).map_err(|error| {
            RenderError::InvalidContent {
                message: error.to_string(),
            }
        })?;
        self.capabilities
            .require(request.required_channels(), camera_count)?;
        let render_started = Instant::now();
        let mut rendered = Vec::with_capacity(request.cameras().len());
        let mut pending = VecDeque::with_capacity(READBACK_RING_CAPACITY);
        for camera in request.cameras() {
            let readback = self
                .begin_camera(camera, request.scene(), request.appearance())
                .map_err(map_scene_error)?;
            pending.push_back((*camera, readback));
            if pending.len() == READBACK_RING_CAPACITY {
                let (completed_camera, completed) =
                    pending.pop_front().expect("the readback ring is nonempty");
                let frame = completed
                    .finish(&self.device)
                    .map_err(|error| map_scene_error(WgpuSceneError::Readback(error)))?;
                rendered.push((completed_camera, frame));
            }
        }
        while let Some((camera, readback)) = pending.pop_front() {
            let frame = readback
                .finish(&self.device)
                .map_err(|error| map_scene_error(WgpuSceneError::Readback(error)))?;
            rendered.push((camera, frame));
        }
        let render_seconds = checked_duration(render_started.elapsed())?;
        let gpu_seconds = rendered
            .iter()
            .try_fold(0.0_f32, |total, (_, frame)| {
                frame.gpu_seconds.map(|seconds| total + seconds)
            })
            .map(|seconds| {
                NonNegativeFinite::try_new(seconds).map_err(|error| RenderError::Internal {
                    message: format!("invalid accumulated GPU timestamp: {error}"),
                })
            })
            .transpose()?;
        let encode_started = Instant::now();
        for (camera, frame) in rendered {
            staging
                .prepare_camera(camera.camera_id())
                .map_err(|error| render_io("prepare camera output", &error))?;
            let size = camera.image_size();
            if request.required_channels().rgb() {
                let rgb = frame
                    .rgba8
                    .chunks_exact(4)
                    .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
                    .collect();
                RgbFrame::try_new(size.width(), size.height(), rgb)
                    .map_err(|error| invalid_frame("RGB", error))?
                    .write_png(&staging.rgb_path(camera.camera_id()))
                    .map_err(|error| render_encoding("RGB PNG", &error))?;
            }
            if request.required_channels().depth() {
                DepthFrame::try_new(size.width(), size.height(), frame.depth)
                    .map_err(|error| invalid_frame("depth", error))?
                    .write_exr(&staging.depth_path(camera.camera_id()))
                    .map_err(|error| render_encoding("depth EXR", &error))?;
            }
            if request.required_channels().instance() {
                InstanceFrame::try_new(size.width(), size.height(), frame.instance)
                    .map_err(|error| invalid_frame("instance", error))?
                    .write_png(&staging.instance_path(camera.camera_id()))
                    .map_err(|error| render_encoding("instance PNG", &error))?;
            }
        }
        let encode_seconds = checked_duration(encode_started.elapsed())?;
        let zero = NonNegativeFinite::try_new(0.0).map_err(|error| RenderError::Internal {
            message: error.to_string(),
        })?;
        let camera_count = NonZeroU32::new(camera_count).ok_or_else(|| RenderError::Internal {
            message: "validated render request unexpectedly contained zero cameras".to_owned(),
        })?;
        Ok(RenderReport::new(
            request.scene().scene_id(),
            self.info.clone(),
            camera_count,
            request.required_channels(),
            RenderStageTimings::new(zero, render_seconds, zero, encode_seconds)
                .with_gpu_seconds(gpu_seconds),
            0,
        ))
    }

    fn flush(&mut self) -> Result<(), RenderError> {
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(30)),
            })
            .map(|_| ())
            .map_err(|error| RenderError::Timeout {
                message: error.to_string(),
            })
    }
}

fn map_scene_error(error: WgpuSceneError) -> RenderError {
    match error {
        WgpuSceneError::NotPrepared => RenderError::NotPrepared,
        other => RenderError::InvalidContent {
            message: other.to_string(),
        },
    }
}

fn checked_duration(duration: Duration) -> Result<NonNegativeFinite, RenderError> {
    NonNegativeFinite::try_new(duration.as_secs_f32()).map_err(|error| RenderError::Internal {
        message: error.to_string(),
    })
}

fn render_io(operation: &str, error: &std::io::Error) -> RenderError {
    RenderError::Io {
        operation: operation.to_owned(),
        message: error.to_string(),
    }
}

fn render_encoding(operation: &str, error: &bpp_dataset::FrameWriteError) -> RenderError {
    RenderError::Io {
        operation: operation.to_owned(),
        message: error.to_string(),
    }
}

fn invalid_frame(channel: &str, error: bpp_dataset::FrameValidationError) -> RenderError {
    RenderError::Internal {
        message: format!("GPU {channel} readback violated frame invariants: {error}"),
    }
}

struct ReadbackBuffer {
    buffer: wgpu::Buffer,
    padded_bytes_per_row: u32,
    unpadded_bytes_per_row: u32,
    height: u32,
}

impl ReadbackBuffer {
    fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        label: &str,
    ) -> Result<Self, WgpuReadbackError> {
        let unpadded_bytes_per_row = width
            .checked_mul(bytes_per_pixel)
            .ok_or(WgpuReadbackError::SizeOverflow)?;
        let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row
            .checked_add(alignment - 1)
            .map(|value| value / alignment * alignment)
            .ok_or(WgpuReadbackError::SizeOverflow)?;
        let size = u64::from(padded_bytes_per_row)
            .checked_mul(u64::from(height))
            .ok_or(WgpuReadbackError::SizeOverflow)?;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        Ok(Self {
            buffer,
            padded_bytes_per_row,
            unpadded_bytes_per_row,
            height,
        })
    }

    fn copy_from(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        extent: wgpu::Extent3d,
    ) {
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bytes_per_row),
                    rows_per_image: Some(self.height),
                },
            },
            extent,
        );
    }

    fn begin_read(&self) -> mpsc::Receiver<Result<(), wgpu::BufferAsyncError>> {
        let slice = self.buffer.slice(..);
        let (sender, receiver) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        receiver
    }

    fn finish_read(
        &self,
        receiver: &mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    ) -> Result<Vec<u8>, WgpuReadbackError> {
        let slice = self.buffer.slice(..);
        receive_mapping(receiver).map_err(map_mapping_completion)?;
        let mapped = slice.get_mapped_range()?;
        let capacity = usize::try_from(self.unpadded_bytes_per_row)
            .ok()
            .and_then(|row| {
                usize::try_from(self.height)
                    .ok()
                    .and_then(|height| row.checked_mul(height))
            })
            .ok_or(WgpuReadbackError::SizeOverflow)?;
        let mut bytes = Vec::with_capacity(capacity);
        let padded = usize::try_from(self.padded_bytes_per_row)
            .map_err(|_| WgpuReadbackError::SizeOverflow)?;
        let unpadded = usize::try_from(self.unpadded_bytes_per_row)
            .map_err(|_| WgpuReadbackError::SizeOverflow)?;
        for row in mapped.chunks_exact(padded) {
            bytes.extend_from_slice(&row[..unpadded]);
        }
        drop(mapped);
        self.buffer.unmap();
        Ok(bytes)
    }
}

/// Failure during bounded GPU readback.
#[derive(Debug, Error)]
enum WgpuReadbackError {
    #[error("render target or readback size overflow")]
    SizeOverflow,
    #[error("GPU polling failed: {0}")]
    Poll(#[from] wgpu::PollError),
    #[error("GPU buffer mapping failed: {0}")]
    Map(#[from] wgpu::BufferAsyncError),
    #[error("GPU mapped range access failed: {0}")]
    MapRange(#[from] wgpu::MapRangeError),
    #[error("GPU mapping callback was dropped")]
    CallbackDropped,
    #[error("GPU instance target returned ID {value} outside the canonical u16 domain")]
    InstanceIdOverflow { value: u32 },
    #[error("GPU timestamp duration exceeded the finite f32 report domain")]
    TimestampOverflow,
}

fn create_target(
    device: &wgpu::Device,
    size: wgpu::Extent3d,
    format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn create_sampled_target(
    device: &wgpu::Device,
    size: wgpu::Extent3d,
    format: wgpu::TextureFormat,
    label: &str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    })
}

fn color_attachment(
    view: &wgpu::TextureView,
    clear: wgpu::Color,
) -> wgpu::RenderPassColorAttachment<'_> {
    wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations {
            load: wgpu::LoadOp::Clear(clear),
            store: wgpu::StoreOp::Store,
        },
    }
}

fn encode_draw_calls<'pass>(
    pass: &mut wgpu::RenderPass<'pass>,
    draw_calls: &'pass [DrawCall<'pass>],
) {
    for call in draw_calls {
        pass.set_vertex_buffer(0, call.primitive.vertex_buffer.slice(..));
        pass.set_index_buffer(
            call.primitive.index_buffer.slice(..),
            wgpu::IndexFormat::Uint32,
        );
        pass.draw_indexed(
            0..call.primitive.index_count,
            0,
            call.draw_index..call.draw_index + 1,
        );
    }
}

fn create_scene_pipeline(
    device: &wgpu::Device,
) -> (
    wgpu::RenderPipeline,
    wgpu::RenderPipeline,
    wgpu::BindGroupLayout,
    wgpu::BindGroupLayout,
    wgpu::BindGroupLayout,
) {
    let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("bpp camera layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let draws_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("bpp draw data layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let shadow_layout = create_shadow_bind_group_layout(device);
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("bpp scene pipeline layout"),
        bind_group_layouts: &[
            Some(&camera_layout),
            Some(&draws_layout),
            Some(&shadow_layout),
        ],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::include_wgsl!("scene.wgsl"));
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("bpp scene pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: 32,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &GPU_VERTEX_ATTRIBUTES,
            })],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[
                Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                }),
                Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R32Float,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                }),
                Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R32Uint,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                }),
            ],
        }),
        multiview_mask: None,
        cache: None,
    });
    let shadow_pipeline = create_shadow_pipeline(device, &camera_layout, &draws_layout);
    (
        pipeline,
        shadow_pipeline,
        camera_layout,
        draws_layout,
        shadow_layout,
    )
}

fn create_shadow_pipeline(
    device: &wgpu::Device,
    camera_layout: &wgpu::BindGroupLayout,
    draws_layout: &wgpu::BindGroupLayout,
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("bpp shadow pipeline layout"),
        bind_group_layouts: &[Some(camera_layout), Some(draws_layout)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::include_wgsl!("shadow.wgsl"));
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("bpp directional shadow pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: 32,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &GPU_VERTEX_ATTRIBUTES,
            })],
        },
        primitive: wgpu::PrimitiveState {
            cull_mode: Some(wgpu::Face::Front),
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::LessEqual),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState {
                constant: 2,
                slope_scale: 2.0,
                clamp: 0.0,
            },
        }),
        multisample: wgpu::MultisampleState::default(),
        fragment: None,
        multiview_mask: None,
        cache: None,
    })
}

fn create_shadow_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("bpp shadow texture layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
        ],
    })
}

fn create_fxaa_pipeline(device: &wgpu::Device) -> (wgpu::RenderPipeline, wgpu::BindGroupLayout) {
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("bpp FXAA layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("bpp FXAA pipeline layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    let shader = device.create_shader_module(wgpu::include_wgsl!("fxaa.wgsl"));
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("bpp RGB FXAA pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[],
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, bind_group_layout)
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn rigid_matrix(transform: RigidTransform) -> glam::Mat4 {
    let translation = transform.translation().map(bpp_core::Meters::get);
    glam::Mat4::from_rotation_translation(
        glam::Quat::from_array(transform.rotation_xyzw()),
        glam::Vec3::from_array(translation),
    )
}

fn rotation_matrix(transform: RigidTransform) -> glam::Mat4 {
    glam::Mat4::from_quat(glam::Quat::from_array(transform.rotation_xyzw()))
}

#[allow(clippy::cast_precision_loss)]
fn dimension_as_f32(value: u32) -> Result<f32, WgpuSceneError> {
    if value > 1 << 24 {
        return Err(WgpuSceneError::ImageDimensionPrecision { value });
    }
    Ok(value as f32)
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
    timestamp_queries: bool,
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
        (
            "timestamp_queries".to_owned(),
            timestamp_queries.to_string(),
        ),
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
    use std::sync::Arc;
    use std::time::Instant;

    use bpp_assets::{
        ConvexPart, NormalizedAsset, ResolvedAsset, ResolvedAssetCatalog, VisualMesh,
        VisualPrimitive,
    };
    use bpp_core::{AssetId, RenderSceneRequest, WgpuAdapterPreference, WgpuDevicePreference};
    use bpp_render_api::Renderer;

    use super::{
        MappingCompletionError, adapter_rank, directional_shadow_matrix, pack_vertices,
        receive_mapping,
    };

    fn project_opencv_point_to_clip(
        point: [f32; 3],
        intrinsics: [f32; 5],
        image_size: [f32; 2],
        clip_planes: [f32; 2],
    ) -> [f32; 4] {
        let [x, y, z] = point;
        let [fx, skew, cx, fy, cy] = intrinsics;
        let [width, height] = image_size;
        let [near, far] = clip_planes;
        [
            2.0 * (fx * x + skew * y) / width + (2.0 * cx / width - 1.0) * z,
            -2.0 * fy * y / height + (1.0 - 2.0 * cy / height) * z,
            far / (far - near) * z - far * near / (far - near),
            z,
        ]
    }

    fn assert_float_array<const N: usize>(actual: [f32; N], expected: [f32; N]) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert_eq!(actual.to_bits(), expected.to_bits());
        }
    }

    fn visible_triangle_catalog() -> ResolvedAssetCatalog {
        let primitive = VisualPrimitive::try_new(
            vec![[-0.6, -0.6, 2.0], [0.6, -0.6, 2.0], [0.0, 0.6, 2.0]],
            vec![[0.0, 0.0, -1.0]; 3],
            vec![],
            vec![0, 1, 2],
            None,
        )
        .unwrap();
        let collision = ConvexPart::try_new(
            vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            vec![0, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3],
        )
        .unwrap();
        let asset = Arc::new(
            NormalizedAsset::try_new(
                VisualMesh::try_new(vec![primitive]).unwrap(),
                vec![],
                vec![],
                vec![collision],
            )
            .unwrap(),
        );
        ResolvedAssetCatalog::try_new(vec![ResolvedAsset::new(
            AssetId::new("triangle").unwrap(),
            *blake3::hash(b"triangle-visible").as_bytes(),
            asset,
        )])
        .unwrap()
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
    fn mapping_failures_and_dropped_callbacks_remain_distinct() {
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(Err("injected map failure")).unwrap();
        assert_eq!(
            receive_mapping(&receiver),
            Err(MappingCompletionError::Mapping("injected map failure"))
        );

        let (sender, receiver) = std::sync::mpsc::channel::<Result<(), &str>>();
        drop(sender);
        assert_eq!(
            receive_mapping(&receiver),
            Err(MappingCompletionError::CallbackDropped)
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
    fn opencv_camera_center_projects_to_the_wgpu_viewport_center() {
        let clip = project_opencv_point_to_clip(
            [0.0, 0.0, 2.0],
            [400.0, 0.0, 320.0, 400.0, 240.0],
            [640.0, 480.0],
            [0.1, 10.0],
        );

        assert_eq!(clip[0].to_bits(), 0.0_f32.to_bits());
        assert_eq!(clip[1].to_bits(), 0.0_f32.to_bits());
        assert!((clip[2] / clip[3] - 0.959_596).abs() <= f32::EPSILON);
        assert_eq!(clip[3].to_bits(), 2.0_f32.to_bits());
    }

    #[test]
    fn opencv_down_axis_maps_to_negative_wgpu_ndc_y_and_skew_affects_x() {
        let clip = project_opencv_point_to_clip(
            [0.25, 0.5, 2.0],
            [400.0, 20.0, 320.0, 400.0, 240.0],
            [640.0, 480.0],
            [0.1, 10.0],
        );
        let ndc = [clip[0] / clip[3], clip[1] / clip[3]];

        assert!((ndc[0] - 0.171_875).abs() <= f32::EPSILON);
        assert!((ndc[1] - (-0.416_666_7)).abs() <= f32::EPSILON);
    }

    #[test]
    fn camera_near_and_far_planes_map_to_wgpu_zero_and_one_depth() {
        for (z, expected) in [(0.1, 0.0), (10.0, 1.0)] {
            let clip = project_opencv_point_to_clip(
                [0.0, 0.0, z],
                [400.0, 0.0, 320.0, 400.0, 240.0],
                [640.0, 480.0],
                [0.1, 10.0],
            );
            assert!((clip[2] / clip[3] - expected).abs() <= 2.0 * f32::EPSILON);
        }
    }

    #[test]
    fn directional_shadow_volume_contains_scene_bounds_for_vertical_and_oblique_lights() {
        let minimum = glam::vec3(-2.0, -1.0, 0.25);
        let maximum = glam::vec3(3.0, 4.0, 2.5);
        for direction in [glam::Vec3::Z, glam::vec3(1.0, -2.0, 3.0).normalize()] {
            let matrix = directional_shadow_matrix(direction, minimum, maximum);
            for x in [minimum.x, maximum.x] {
                for y in [minimum.y, maximum.y] {
                    for z in [minimum.z, maximum.z] {
                        let clip = matrix * glam::vec4(x, y, z, 1.0);
                        let ndc = clip.truncate() / clip.w;
                        assert!(ndc.x.abs() <= 1.0 + 4.0 * f32::EPSILON);
                        assert!(ndc.y.abs() <= 1.0 + 4.0 * f32::EPSILON);
                        assert!((0.0..=1.0 + 4.0 * f32::EPSILON).contains(&ndc.z));
                    }
                }
            }
        }
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

    #[test]
    #[ignore = "manual hardware-dependent offscreen readback smoke test"]
    fn offscreen_clear_targets_round_trip_all_canonical_channels() {
        let config = bpp_core::WgpuRendererConfig::try_new(
            WgpuDevicePreference::Automatic,
            WgpuAdapterPreference::HighPerformance,
            16,
            true,
        )
        .unwrap();
        let renderer = super::WgpuRenderer::try_new(config).unwrap();
        let frame = renderer
            .render_clear_targets(2, 2, [0.25, 0.5, 0.75, 1.0])
            .unwrap();

        assert_eq!(frame.rgba8, [64, 127, 191, 255].repeat(4));
        assert_float_array(frame.depth[0..4].try_into().unwrap(), [0.0; 4]);
        assert_eq!(frame.instance, vec![0; 4]);
    }

    #[test]
    #[ignore = "manual hardware-dependent triangle render/readback test"]
    fn triangle_render_produces_aligned_rgb_metric_depth_and_instance_pixels() {
        const REQUEST: &str = r#"{"schema_version":1,"scene":{"schema_version":1,"scene_id":1,"accepted_attempt":0,"seed_fingerprint":"seed","bin":{"schema_version":1,"inner_dimensions_m":[1.0,1.0,1.0],"floor_thickness_m":0.1,"wall_thickness_m":0.1,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel"},"instances":[{"schema_version":1,"instance_id":1,"asset_id":"triangle","world_from_model":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"physical_properties":{"schema_version":1,"mass_kg":1.0,"friction":0.5,"restitution":0.0}}],"physics":{"schema_version":1,"attempts":1,"steps":1,"simulated_seconds":0.01,"final_max_linear_speed_mps":0.0,"final_max_angular_speed_rad_s":0.0,"final_max_penetration_m":0.0,"rejection_counts":[]}},"cameras":[{"schema_version":1,"camera_id":0,"image_size":{"width":32,"height":32},"k":[25.0,0.0,16.0,0.0,25.0,16.0,0.0,0.0,1.0],"camera_from_world":{"camera_from_world":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"near_m":0.1,"far_m":10.0}],"appearance":{"schema_version":1,"background_srgb":[0.0,0.0,0.0,1.0],"ambient_light":{"schema_version":1,"color":[1.0,1.0,1.0],"intensity":1.0},"directional_lights":[],"point_lights":[]},"required_channels":{"schema_version":1,"rgb":true,"depth":true,"instance":true}}"#;
        let mut request_json: serde_json::Value = serde_json::from_str(REQUEST).unwrap();
        request_json["appearance"]["ambient_light"]["intensity"] = serde_json::Value::from(0.1);
        request_json["appearance"]["directional_lights"] = serde_json::json!([{
            "schema_version": 1,
            "direction_world": [0.0, 0.0, 1.0],
            "color": [1.0, 1.0, 1.0],
            "illuminance_lux": 1000.0
        }]);
        let request: RenderSceneRequest = serde_json::from_value(request_json).unwrap();
        let config = bpp_core::WgpuRendererConfig::try_new(
            WgpuDevicePreference::Automatic,
            WgpuAdapterPreference::HighPerformance,
            16,
            true,
        )
        .unwrap();
        let mut renderer = super::WgpuRenderer::try_new(config).unwrap();
        renderer.prepare(&visible_triangle_catalog()).unwrap();
        let frame = renderer
            .render_camera(&request.cameras()[0], request.scene(), request.appearance())
            .unwrap();

        let foreground: Vec<_> = frame
            .instance
            .iter()
            .enumerate()
            .filter_map(|(index, id)| (*id == 1).then_some(index))
            .collect();
        assert!(!foreground.is_empty());
        for index in foreground {
            assert!((frame.depth[index] - 2.0).abs() <= 1.0e-5);
            assert_ne!(&frame.rgba8[index * 4..index * 4 + 3], &[0, 0, 0]);
        }
        let mut samples = Vec::with_capacity(31);
        for _ in 0..31 {
            let started = Instant::now();
            renderer
                .render_camera(&request.cameras()[0], request.scene(), request.appearance())
                .unwrap();
            samples.push(started.elapsed());
        }
        samples.sort_unstable();
        eprintln!("wgpu triangle render+readback median: {:?}", samples[15]);
    }

    #[test]
    #[ignore = "manual hardware-dependent transactional dataset acceptance test"]
    fn renderer_pipeline_commits_a_validated_canonical_scene() {
        const REQUEST: &str = r#"{"schema_version":1,"scene":{"schema_version":1,"scene_id":1,"accepted_attempt":0,"seed_fingerprint":"seed","bin":{"schema_version":1,"inner_dimensions_m":[1.0,1.0,1.0],"floor_thickness_m":0.1,"wall_thickness_m":0.1,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel"},"instances":[{"schema_version":1,"instance_id":1,"asset_id":"triangle","world_from_model":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"physical_properties":{"schema_version":1,"mass_kg":1.0,"friction":0.5,"restitution":0.0}}],"physics":{"schema_version":1,"attempts":1,"steps":1,"simulated_seconds":0.01,"final_max_linear_speed_mps":0.0,"final_max_angular_speed_rad_s":0.0,"final_max_penetration_m":0.0,"rejection_counts":[]}},"cameras":[{"schema_version":1,"camera_id":0,"image_size":{"width":32,"height":32},"k":[25.0,0.0,16.0,0.0,25.0,16.0,0.0,0.0,1.0],"camera_from_world":{"camera_from_world":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"near_m":0.1,"far_m":10.0}],"appearance":{"schema_version":1,"background_srgb":[0.0,0.0,0.0,1.0],"ambient_light":{"schema_version":1,"color":[1.0,1.0,1.0],"intensity":1.0},"directional_lights":[],"point_lights":[]},"required_channels":{"schema_version":1,"rgb":true,"depth":true,"instance":true}}"#;
        let mut request_json: serde_json::Value = serde_json::from_str(REQUEST).unwrap();
        let template = request_json["cameras"][0].clone();
        request_json["cameras"] = serde_json::Value::Array(
            (0..3)
                .map(|camera_id| {
                    let mut camera = template.clone();
                    camera["camera_id"] = serde_json::Value::from(camera_id);
                    camera
                })
                .collect(),
        );
        let request: RenderSceneRequest = serde_json::from_value(request_json).unwrap();
        let config = bpp_core::WgpuRendererConfig::try_new(
            WgpuDevicePreference::Automatic,
            WgpuAdapterPreference::HighPerformance,
            16,
            false,
        )
        .unwrap();
        let mut renderer = super::WgpuRenderer::try_new(config).unwrap();
        let root = tempfile::tempdir().unwrap();
        renderer.prepare(&visible_triangle_catalog()).unwrap();
        let staging =
            bpp_dataset::SceneStagingArea::create(root.path(), request.scene().scene_id()).unwrap();
        std::fs::write(
            staging.scene_metadata_path(),
            serde_json::to_vec(request.scene()).unwrap(),
        )
        .unwrap();
        for camera in request.cameras() {
            staging.prepare_camera(camera.camera_id()).unwrap();
            std::fs::write(
                staging.frame_metadata_path(camera.camera_id()),
                serde_json::to_vec(camera).unwrap(),
            )
            .unwrap();
        }
        let report = Renderer::render_scene(&mut renderer, &request, &staging).unwrap();
        let timestamps_supported = report.backend().properties()["timestamp_queries"] == "true";
        assert_eq!(
            report.timings().gpu_seconds().is_some(),
            timestamps_supported
        );
        if let Some(gpu_seconds) = report.timings().gpu_seconds() {
            assert!(gpu_seconds.get() > 0.0);
            assert!(gpu_seconds.get() <= report.timings().render_seconds().get());
        }
        bpp_dataset::validate_and_write_checksums(&staging, &request).unwrap();
        assert!(staging.root().join("checksums.json").is_file());
        for camera in request.cameras() {
            assert!(staging.rgb_path(camera.camera_id()).is_file());
            assert!(staging.depth_path(camera.camera_id()).is_file());
            assert!(staging.instance_path(camera.camera_id()).is_file());
        }

        let failed_staging =
            bpp_dataset::SceneStagingArea::create(root.path(), request.scene().scene_id()).unwrap();
        super::record_device_loss(&renderer.device_loss, "injected test loss".to_owned());
        let error = Renderer::render_scene(&mut renderer, &request, &failed_staging).unwrap_err();
        assert!(error.to_string().contains("injected test loss"));
        for camera in request.cameras() {
            assert!(!failed_staging.rgb_path(camera.camera_id()).exists());
            assert!(!failed_staging.depth_path(camera.camera_id()).exists());
            assert!(!failed_staging.instance_path(camera.camera_id()).exists());
        }
    }
}

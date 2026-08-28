//! Headless wgpu renderer implementation.

use std::collections::BTreeMap;
use std::{
    num::NonZeroU32,
    sync::mpsc,
    time::{Duration, Instant},
};

use bpp_assets::{ResolvedAssetCatalog, VisualPrimitive};
use bpp_core::{
    BackendInfo, BackendKind, CameraSnapshot, InvalidBackendInfoError, NonNegativeFinite,
    RenderChannels, RenderError, RenderReport, RenderSceneRequest, RenderStageTimings, Renderer,
    RendererCapabilities, RigidTransform, SceneSnapshot, WgpuAdapterPreference,
    WgpuDevicePreference, WgpuRendererConfig,
};
use bpp_dataset::{DepthFrame, InstanceFrame, RgbFrame, SceneStagingArea};
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
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawUniform {
    world_from_model: [[f32; 4]; 4],
    normal_from_model: [[f32; 4]; 4],
    base_color: [f32; 4],
    instance_id: u32,
    padding: [u32; 3],
}

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
    camera_layout: wgpu::BindGroupLayout,
    draws_layout: wgpu::BindGroupLayout,
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
}

struct ReadbackFrame {
    rgba8: Vec<u8>,
    depth: Vec<f32>,
    instance: Vec<u16>,
}

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
        for format in [
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::R32Float,
            wgpu::TextureFormat::R16Uint,
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
        let (render_pipeline, camera_layout, draws_layout) = create_scene_pipeline(&device);
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
            queue,
            render_pipeline,
            camera_layout,
            draws_layout,
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
                let base_color = primitive
                    .material()
                    .and_then(|index| usize::try_from(index.get()).ok())
                    .and_then(|index| entry.asset().materials().get(index))
                    .map_or([1.0; 4], bpp_assets::PbrMaterial::base_color_factor);
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
                    vertex_buffer,
                    index_buffer,
                    index_count,
                    base_color,
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

    fn render_clear_targets(
        &self,
        width: u32,
        height: u32,
        background_srgb: [f32; 4],
    ) -> Result<ReadbackFrame, WgpuReadbackError> {
        self.render_targets(width, height, background_srgb, None, &[], &[])
    }

    fn render_targets(
        &self,
        width: u32,
        height: u32,
        background_srgb: [f32; 4],
        camera: Option<&CameraUniform>,
        draw_uniforms: &[DrawUniform],
        draw_calls: &[DrawCall<'_>],
    ) -> Result<ReadbackFrame, WgpuReadbackError> {
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
            wgpu::TextureFormat::R16Uint,
            "instance",
        );
        let rgb_view = rgb.create_view(&wgpu::TextureViewDescriptor::default());
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
        let instance_readback = ReadbackBuffer::new(&self.device, width, height, 2, "instance")?;
        let linear = background_srgb.map(srgb_to_linear);
        let bindings = camera
            .zip((!draw_uniforms.is_empty()).then_some(draw_uniforms))
            .map(|(camera, draws)| self.create_scene_bindings(camera, draws));

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("bpp clear targets"),
            });
        self.encode_scene_pass(
            &mut encoder,
            &TargetViews {
                rgb: &rgb_view,
                depth: &depth_view,
                instance: &instance_view,
                depth_test: &depth_test_view,
            },
            linear,
            bindings.as_ref(),
            draw_calls,
        );
        rgb_readback.copy_from(&mut encoder, &rgb, extent);
        depth_readback.copy_from(&mut encoder, &depth, extent);
        instance_readback.copy_from(&mut encoder, &instance, extent);
        let submission = self.queue.submit([encoder.finish()]);
        let rgba8 = rgb_readback.read(&self.device, Some(submission.clone()))?;
        let depth_bytes = depth_readback.read(&self.device, Some(submission.clone()))?;
        let instance_bytes = instance_readback.read(&self.device, Some(submission))?;
        let depth = depth_bytes
            .chunks_exact(4)
            .map(|bytes| f32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
            .collect();
        let instance = instance_bytes
            .chunks_exact(2)
            .map(|bytes| u16::from_ne_bytes([bytes[0], bytes[1]]))
            .collect();
        Ok(ReadbackFrame {
            rgba8,
            depth,
            instance,
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

    fn encode_scene_pass(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        targets: &TargetViews<'_>,
        clear: [f32; 4],
        bindings: Option<&SceneBindings>,
        draw_calls: &[DrawCall<'_>],
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("bpp scene pass"),
            color_attachments: &[
                Some(color_attachment(
                    targets.rgb,
                    wgpu::Color {
                        r: f64::from(clear[0]),
                        g: f64::from(clear[1]),
                        b: f64::from(clear[2]),
                        a: f64::from(clear[3]),
                    },
                )),
                Some(color_attachment(targets.depth, wgpu::Color::TRANSPARENT)),
                Some(color_attachment(targets.instance, wgpu::Color::TRANSPARENT)),
            ],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: targets.depth_test,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(bindings) = bindings {
            pass.set_pipeline(&self.render_pipeline);
            pass.set_bind_group(0, &bindings.camera, &[]);
            pass.set_bind_group(1, &bindings.draws, &[]);
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
    }

    fn render_camera(
        &self,
        camera: &CameraSnapshot,
        scene: &SceneSnapshot,
        background_srgb: [f32; 4],
    ) -> Result<ReadbackFrame, WgpuSceneError> {
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
                .render_clear_targets(size.width(), size.height(), background_srgb)
                .map_err(WgpuSceneError::Readback);
        }
        let intrinsics = camera.intrinsics();
        let camera_uniform = CameraUniform {
            camera_from_world: rigid_matrix(camera.camera_from_world().camera_from_world())
                .to_cols_array_2d(),
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
        };
        self.render_targets(
            size.width(),
            size.height(),
            background_srgb,
            Some(&camera_uniform),
            &uniforms,
            &calls,
        )
        .map_err(WgpuSceneError::Readback)
    }
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
    #[error(transparent)]
    Readback(#[from] WgpuReadbackError),
}

impl Renderer for WgpuRenderer {
    type AssetCatalog = ResolvedAssetCatalog;
    type StagingArea = SceneStagingArea;

    fn info(&self) -> BackendInfo {
        WgpuRenderer::info(self)
    }

    fn capabilities(&self) -> RendererCapabilities {
        WgpuRenderer::capabilities(self)
    }

    fn prepare(&mut self, assets: &Self::AssetCatalog) -> Result<(), RenderError> {
        WgpuRenderer::prepare(self, assets)
            .map(|_| ())
            .map_err(|error| RenderError::InvalidContent {
                message: error.to_string(),
            })
    }

    fn render_scene(
        &mut self,
        request: &RenderSceneRequest,
        staging: &Self::StagingArea,
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
        for camera in request.cameras() {
            let frame = self
                .render_camera(
                    camera,
                    request.scene(),
                    request.appearance().background_srgb().rgba(),
                )
                .map_err(map_scene_error)?;
            rendered.push((*camera, frame));
        }
        let render_seconds = checked_duration(render_started.elapsed())?;
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
            RenderStageTimings::new(zero, render_seconds, zero, encode_seconds),
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

    fn read(
        &self,
        device: &wgpu::Device,
        submission: Option<wgpu::SubmissionIndex>,
    ) -> Result<Vec<u8>, WgpuReadbackError> {
        let slice = self.buffer.slice(..);
        let (sender, receiver) = mpsc::sync_channel(1);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        device.poll(wgpu::PollType::Wait {
            submission_index: submission,
            timeout: Some(Duration::from_secs(30)),
        })?;
        receiver
            .recv()
            .map_err(|_| WgpuReadbackError::CallbackDropped)??;
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

fn create_scene_pipeline(
    device: &wgpu::Device,
) -> (
    wgpu::RenderPipeline,
    wgpu::BindGroupLayout,
    wgpu::BindGroupLayout,
) {
    const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x2];
    let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("bpp camera layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
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
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("bpp scene pipeline layout"),
        bind_group_layouts: &[Some(&camera_layout), Some(&draws_layout)],
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
                attributes: &ATTRIBUTES,
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
                    format: wgpu::TextureFormat::R16Uint,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                }),
            ],
        }),
        multiview_mask: None,
        cache: None,
    });
    (pipeline, camera_layout, draws_layout)
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
    use std::sync::Arc;

    use bpp_assets::{
        ConvexPart, NormalizedAsset, ResolvedAsset, ResolvedAssetCatalog, VisualMesh,
        VisualPrimitive,
    };
    use bpp_core::{AssetId, RenderSceneRequest, WgpuAdapterPreference, WgpuDevicePreference};

    use super::{adapter_rank, pack_vertices};

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

    #[test]
    #[ignore = "manual hardware-dependent offscreen readback smoke test"]
    fn offscreen_clear_targets_round_trip_all_canonical_channels() {
        let config = bpp_core::WgpuRendererConfig::try_new(
            WgpuDevicePreference::Automatic,
            WgpuAdapterPreference::HighPerformance,
            16,
            false,
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
        let request: RenderSceneRequest = serde_json::from_str(REQUEST).unwrap();
        let config = bpp_core::WgpuRendererConfig::try_new(
            WgpuDevicePreference::Automatic,
            WgpuAdapterPreference::HighPerformance,
            16,
            false,
        )
        .unwrap();
        let mut renderer = super::WgpuRenderer::try_new(config).unwrap();
        renderer.prepare(&visible_triangle_catalog()).unwrap();
        let frame = renderer
            .render_camera(
                &request.cameras()[0],
                request.scene(),
                request.appearance().background_srgb().rgba(),
            )
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
    }

    #[test]
    #[ignore = "manual hardware-dependent transactional dataset acceptance test"]
    fn renderer_pipeline_commits_a_validated_canonical_scene() {
        const REQUEST: &str = r#"{"schema_version":1,"scene":{"schema_version":1,"scene_id":1,"accepted_attempt":0,"seed_fingerprint":"seed","bin":{"schema_version":1,"inner_dimensions_m":[1.0,1.0,1.0],"floor_thickness_m":0.1,"wall_thickness_m":0.1,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel"},"instances":[{"schema_version":1,"instance_id":1,"asset_id":"triangle","world_from_model":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"physical_properties":{"schema_version":1,"mass_kg":1.0,"friction":0.5,"restitution":0.0}}],"physics":{"schema_version":1,"attempts":1,"steps":1,"simulated_seconds":0.01,"final_max_linear_speed_mps":0.0,"final_max_angular_speed_rad_s":0.0,"final_max_penetration_m":0.0,"rejection_counts":[]}},"cameras":[{"schema_version":1,"camera_id":0,"image_size":{"width":32,"height":32},"k":[25.0,0.0,16.0,0.0,25.0,16.0,0.0,0.0,1.0],"camera_from_world":{"camera_from_world":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]}},"near_m":0.1,"far_m":10.0}],"appearance":{"schema_version":1,"background_srgb":[0.0,0.0,0.0,1.0],"ambient_light":{"schema_version":1,"color":[1.0,1.0,1.0],"intensity":1.0},"directional_lights":[],"point_lights":[]},"required_channels":{"schema_version":1,"rgb":true,"depth":true,"instance":true}}"#;
        let request: RenderSceneRequest = serde_json::from_str(REQUEST).unwrap();
        let config = bpp_core::WgpuRendererConfig::try_new(
            WgpuDevicePreference::Automatic,
            WgpuAdapterPreference::HighPerformance,
            16,
            false,
        )
        .unwrap();
        let renderer = super::WgpuRenderer::try_new(config).unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut pipeline = bpp_pipeline::SerialDatasetRenderer::new(renderer, root.path());
        pipeline.prepare(&visible_triangle_catalog()).unwrap();
        let completed = pipeline.render_and_commit(&request).unwrap();
        assert!(completed.scene_path().join("checksums.json").is_file());
    }
}

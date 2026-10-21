//! Headless wgpu renderer implementation.

use std::collections::{BTreeMap, VecDeque};
use std::{
    num::NonZeroU32,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use bop_assets::{ResolvedAssetCatalog, VisualPrimitive, analytic_bin_snapshot};
use bop_core::{
    AppearanceSnapshot, BackendInfo, BackendKind, CameraSnapshot, InvalidBackendInfoError,
    NonNegativeFinite, RenderChannels, RenderError, RenderReport, RenderSceneRequest,
    RenderStageTimings, RendererCapabilities, RigidTransform, SceneSnapshot, WgpuAdapterPreference,
    WgpuDevicePreference, WgpuRendererConfig,
};
use bop_dataset::{DepthFrame, InstanceFrame, RgbFrame, SceneStagingArea};
use bop_render_api::Renderer;
use thiserror::Error;
use wgpu::util::DeviceExt as _;

mod environment;
mod errors;
mod geometry;
mod gpu_types;
mod pipelines;
mod readback;
mod textures;

pub use errors::*;
#[allow(
    clippy::wildcard_imports,
    reason = "submodules share the crate root's imports and helpers"
)]
use geometry::*;
#[allow(
    clippy::wildcard_imports,
    reason = "submodules share the crate root's imports and helpers"
)]
use gpu_types::*;
#[allow(
    clippy::wildcard_imports,
    reason = "submodules share the crate root's imports and helpers"
)]
use pipelines::*;
#[allow(
    clippy::wildcard_imports,
    reason = "submodules share the crate root's imports and helpers"
)]
use readback::*;

/// Single-owner headless `wgpu` device context.
pub struct WgpuRenderer {
    config: WgpuRendererConfig,
    info: BackendInfo,
    capabilities: RendererCapabilities,
    _instance: wgpu::Instance,
    _adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipelines: ScenePipelines,
    occlusion: OcclusionPipelines,
    post: PostPipelines,
    /// Material for untextured primitives: a white 1x1 texture.
    white_material: wgpu::BindGroup,
    /// The prepared catalog's environment map, or a black stand-in.
    environment: environment::GpuEnvironment,
    timestamp_queries: bool,
    timestamp_period_ns: f32,
    device_loss: Arc<Mutex<Option<String>>>,
    prepared: Option<PreparedGpuCatalog>,
    unit_cube: GpuPrimitive,
}

struct PreparedGpuCatalog {
    fingerprint: String,
    assets: BTreeMap<bop_core::AssetId, GpuAsset>,
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
    /// Whether the asset file assigns this primitive a material.
    has_file_material: bool,
    bounds_min: [f32; 3],
    bounds_max: [f32; 3],
    /// Base-color texture and sampler; `None` samples white.
    material: Option<wgpu::BindGroup>,
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
    camera_buffer: wgpu::Buffer,
    _draws_buffer: wgpu::Buffer,
    camera: wgpu::BindGroup,
    draws: wgpu::BindGroup,
}

/// Per-frame ambient-occlusion targets and the bind groups that read them.
struct OcclusionResources {
    normal_view: wgpu::TextureView,
    camera_z_view: wgpu::TextureView,
    raw_view: wgpu::TextureView,
    blurred_view: wgpu::TextureView,
    occlusion_group: wgpu::BindGroup,
    blur_group: wgpu::BindGroup,
}

/// The main pass's targets: with geometry, shading goes to HDR buffers the
/// reflection pass composites; an empty frame is just the background.
fn main_color_attachments<'a>(
    inputs: &ScenePassInputs<'a>,
    background: wgpu::Color,
) -> Vec<Option<wgpu::RenderPassColorAttachment<'a>>> {
    let mut attachments = match inputs.reflection {
        Some(reflection) => vec![
            Some(color_attachment(&reflection.base_view, background)),
            Some(color_attachment(
                &reflection.reflectance_view,
                wgpu::Color::TRANSPARENT,
            )),
        ],
        None => vec![Some(color_attachment(inputs.targets.rgb, background))],
    };
    attachments.push(Some(color_attachment(
        inputs.targets.depth,
        wgpu::Color::TRANSPARENT,
    )));
    attachments.push(Some(color_attachment(
        inputs.targets.instance,
        wgpu::Color::TRANSPARENT,
    )));
    attachments
}

/// Pipelines after the main pass: the lit scene's mip chain and reflections.
struct PostPipelines {
    reflection: wgpu::RenderPipeline,
    reflection_layout: wgpu::BindGroupLayout,
    downsample: wgpu::RenderPipeline,
    downsample_layout: wgpu::BindGroupLayout,
    /// Clamped trilinear sampling of the lit scene's mip chain.
    screen_sampler: wgpu::Sampler,
    fxaa: wgpu::RenderPipeline,
    fxaa_layout: wgpu::BindGroupLayout,
    fxaa_sampler: wgpu::Sampler,
}

impl PostPipelines {
    fn new(device: &wgpu::Device) -> Self {
        let (reflection, reflection_layout) = create_reflection_pipeline(device);
        let (downsample, downsample_layout) = create_downsample_pipeline(device);
        let (fxaa, fxaa_layout) = create_fxaa_pipeline(device);
        let fxaa_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bop FXAA sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            fxaa,
            fxaa_layout,
            fxaa_sampler,
            reflection,
            reflection_layout,
            downsample,
            downsample_layout,
            screen_sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("bop lit scene sampler"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                mipmap_filter: wgpu::MipmapFilterMode::Linear,
                ..Default::default()
            }),
        }
    }
}

/// Per-frame HDR targets the main pass writes and the reflection pass reads.
struct ReflectionResources {
    /// Level 0 of the lit scene, which the main pass renders into.
    base_view: wgpu::TextureView,
    reflectance_view: wgpu::TextureView,
    group: wgpu::BindGroup,
    /// Downsample targets and the bind groups reading the level above each.
    mip_levels: Vec<(wgpu::TextureView, wgpu::BindGroup)>,
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

impl TimestampResources {
    /// Copies the frame's two timestamps to the mappable buffer.
    fn encode_resolve(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.resolve_query_set(&self.query_set, 0..2, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.readback, 0, 16);
    }
}

struct ScenePassInputs<'a> {
    targets: TargetViews<'a>,
    clear: [f32; 4],
    bindings: Option<&'a SceneBindings>,
    draw_calls: &'a [DrawCall<'a>],
    shadow: Option<&'a ShadowResources>,
    occlusion: Option<&'a OcclusionResources>,
    reflection: Option<&'a ReflectionResources>,
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
        Self::try_new_for_worker(config, 0)
    }

    /// Like [`Self::try_new`], for renderer worker `worker_index` of a run.
    ///
    /// When several adapters tie for the best match on the same backend, such
    /// as two identical GPUs, worker `i` takes adapter `i` modulo their count,
    /// so the workers spread over them. Worker 0 always gets the adapter
    /// [`Self::try_new`] selects.
    ///
    /// # Errors
    ///
    /// Returns a typed error when no matching adapter exists or device
    /// creation fails.
    pub fn try_new_for_worker(
        config: WgpuRendererConfig,
        worker_index: u32,
    ) -> Result<Self, WgpuInitializationError> {
        let instance = wgpu::Instance::default();
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
        let mut candidates: Vec<_> = adapters
            .into_iter()
            .filter_map(|adapter| {
                let info = adapter.get_info();
                let rank = adapter_rank(
                    config.device_preference(),
                    config.adapter_preference(),
                    info.device_type,
                )?;
                let key = AdapterKey {
                    rank,
                    backend_rank: backend_rank(info.backend),
                    vendor: info.vendor,
                    device: info.device,
                    name: info.name.clone(),
                    backend: format!("{:?}", info.backend),
                };
                Some((key, adapter, info))
            })
            .collect();
        let keys: Vec<_> = candidates.iter().map(|(key, _, _)| key.clone()).collect();
        let (_, adapter, adapter_info) = worker_adapter(&keys, worker_index)
            .map(|index| candidates.swap_remove(index))
            .ok_or(WgpuInitializationError::NoMatchingAdapter {
                device_preference: config.device_preference(),
                adapter_preference: config.adapter_preference(),
            })?;

        let timestamp_queries = adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        let descriptor = wgpu::DeviceDescriptor {
            label: Some("bop-render-wgpu"),
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
        let pipelines = create_scene_pipeline(&device);
        let occlusion = create_occlusion_pipelines(&device);
        let post = PostPipelines::new(&device);
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
            unit_cube: create_unit_cube(&device),
            white_material: textures::white_material(&device, &queue, &pipelines.material_layout),
            environment: environment::GpuEnvironment::empty(&device, &queue),
            device,
            queue,
            pipelines,
            occlusion,
            post,
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
            let mut texture_views = BTreeMap::new();
            for (primitive_index, primitive) in
                entry.asset().visual_mesh().primitives().iter().enumerate()
            {
                let material = primitive
                    .material()
                    .and_then(|index| usize::try_from(index.get()).ok())
                    .and_then(|index| entry.asset().materials().get(index));
                let binding = material.and_then(bop_assets::PbrMaterial::base_color_texture);
                let uv_set = binding.map_or(0, |binding| binding.tex_coord_set() as usize);
                let vertices = pack_vertices(primitive, uv_set);
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
                let material_group = binding
                    .map(|binding| self.material_group(entry, binding, &mut texture_views))
                    .transpose()?;
                let base_color =
                    material.map_or([1.0; 4], bop_assets::PbrMaterial::base_color_factor);
                let metallic = material.map_or(0.0, bop_assets::PbrMaterial::metallic_factor);
                let roughness = material.map_or(1.0, bop_assets::PbrMaterial::roughness_factor);
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
                            label: Some("bop asset vertices"),
                            contents: vertex_bytes,
                            usage: wgpu::BufferUsages::VERTEX,
                        });
                let index_buffer =
                    self.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("bop asset indices"),
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
                    has_file_material: material.is_some(),
                    bounds_min,
                    bounds_max,
                    material: material_group,
                });
            }
            assets.insert(entry.asset_id().clone(), GpuAsset { primitives });
        }
        self.environment =
            environment::GpuEnvironment::for_catalog(&self.device, &self.queue, catalog);
        self.prepared = Some(PreparedGpuCatalog {
            fingerprint: catalog.fingerprint().to_owned(),
            assets,
            stats,
        });
        Ok(stats)
    }

    /// The bind group sampling `binding`'s texture, uploading the texture the
    /// first time any primitive of the asset uses it.
    fn material_group(
        &self,
        entry: &bop_assets::ResolvedAsset,
        binding: bop_assets::TextureBinding,
        texture_views: &mut BTreeMap<usize, wgpu::TextureView>,
    ) -> Result<wgpu::BindGroup, WgpuPreparationError> {
        let index = binding.texture().get() as usize;
        let texture = entry.asset().textures().get(index).ok_or_else(|| {
            WgpuPreparationError::MissingTexture {
                asset_id: entry.asset_id().clone(),
                texture_index: index,
            }
        })?;
        let [width, height] = texture.image().dimensions();
        let maximum = self.device.limits().max_texture_dimension_2d;
        if width.max(height) > maximum {
            return Err(WgpuPreparationError::TextureTooLarge {
                asset_id: entry.asset_id().clone(),
                texture_index: index,
                width,
                height,
                maximum,
            });
        }
        let view = texture_views
            .entry(index)
            .or_insert_with(|| textures::upload_texture(&self.device, &self.queue, texture));
        let sampler = self
            .device
            .create_sampler(&textures::sampler_descriptor(texture.sampler()));
        Ok(textures::material_bind_group(
            &self.device,
            &self.pipelines.material_layout,
            view,
            &sampler,
        ))
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
        let fxaa_source_view = self.config.rgb_fxaa().then(|| {
            create_sampled_target(
                &self.device,
                extent,
                wgpu::TextureFormat::Rgba8UnormSrgb,
                "FXAA source",
            )
            .create_view(&wgpu::TextureViewDescriptor::default())
        });
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
        let (bindings, occlusion, shadow, reflection) =
            self.create_frame_resources(extent, camera, draw_uniforms);
        let timestamp = self.create_timestamp_resources();

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("bop clear targets"),
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
                occlusion: occlusion.as_ref(),
                reflection: reflection.as_ref(),
                timestamp_query_set: timestamp.as_ref().map(|resources| &resources.query_set),
            },
        );
        if let Some(source_view) = &fxaa_source_view {
            self.encode_fxaa_pass(
                &mut encoder,
                source_view,
                &rgb_view,
                width,
                height,
                timestamp.as_ref().map(|resources| &resources.query_set),
            );
        }
        if let Some(timestamp) = &timestamp {
            timestamp.encode_resolve(&mut encoder);
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

    /// Uniforms, occlusion targets and the shadow map for one frame; none
    /// when there is nothing to draw.
    fn create_frame_resources(
        &self,
        extent: wgpu::Extent3d,
        camera: Option<&CameraUniform>,
        draw_uniforms: &[DrawUniform],
    ) -> (
        Option<SceneBindings>,
        Option<OcclusionResources>,
        Option<ShadowResources>,
        Option<ReflectionResources>,
    ) {
        let bindings = camera
            .zip((!draw_uniforms.is_empty()).then_some(draw_uniforms))
            .map(|(camera, draws)| self.create_scene_bindings(camera, draws));
        let occlusion = bindings
            .as_ref()
            .map(|bindings| self.create_occlusion_resources(extent, bindings));
        let shadow = occlusion
            .as_ref()
            .map(|occlusion| self.create_shadow_resources(&occlusion.blurred_view));
        let reflection = bindings
            .as_ref()
            .zip(occlusion.as_ref())
            .map(|(bindings, occlusion)| {
                self.create_reflection_resources(extent, bindings, occlusion)
            });
        (bindings, occlusion, shadow, reflection)
    }

    fn create_reflection_resources(
        &self,
        extent: wgpu::Extent3d,
        bindings: &SceneBindings,
        occlusion: &OcclusionResources,
    ) -> ReflectionResources {
        // Enough levels to blur across a sixteenth of the image.
        let levels = 5.min(extent.width.min(extent.height).ilog2() + 1);
        let base = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lit scene before reflections"),
            size: extent,
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let level_view = |level| {
            base.create_view(&wgpu::TextureViewDescriptor {
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        let base_view = level_view(0);
        let mip_levels = (1..levels)
            .map(|level| {
                let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("bop downsample bind group"),
                    layout: &self.post.downsample_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&level_view(level - 1)),
                    }],
                });
                (level_view(level), group)
            })
            .collect();
        let all_levels = base.create_view(&wgpu::TextureViewDescriptor::default());
        let reflectance_view = create_sampled_target(
            &self.device,
            extent,
            wgpu::TextureFormat::Rgba16Float,
            "specular weight and roughness",
        )
        .create_view(&wgpu::TextureViewDescriptor::default());
        let texture = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bop reflection bind group"),
            layout: &self.post.reflection_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: bindings.camera_buffer.as_entire_binding(),
                },
                texture(1, &all_levels),
                texture(2, &reflectance_view),
                texture(3, &occlusion.normal_view),
                texture(4, &occlusion.camera_z_view),
                texture(5, &self.environment.view),
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Sampler(&self.environment.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::Sampler(&self.post.screen_sampler),
                },
                texture(8, &occlusion.blurred_view),
            ],
        });
        ReflectionResources {
            base_view,
            reflectance_view,
            group,
            mip_levels,
        }
    }

    fn create_scene_bindings(
        &self,
        camera: &CameraUniform,
        draws: &[DrawUniform],
    ) -> SceneBindings {
        let camera_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bop camera uniform"),
                contents: bytemuck::bytes_of(camera),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let draws_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bop draw storage"),
                contents: bytemuck::cast_slice(draws),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let camera = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bop camera bind group"),
            layout: &self.pipelines.camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });
        let draws = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bop draws bind group"),
            layout: &self.pipelines.draws_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: draws_buffer.as_entire_binding(),
            }],
        });
        SceneBindings {
            camera_buffer,
            _draws_buffer: draws_buffer,
            camera,
            draws,
        }
    }

    fn create_timestamp_resources(&self) -> Option<TimestampResources> {
        self.timestamp_queries.then(|| TimestampResources {
            query_set: self.device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("bop render timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: 2,
            }),
            resolve: self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bop timestamp resolve"),
                size: 16,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            readback: self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bop timestamp readback"),
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
                label: Some("bop directional shadow pass"),
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
            shadow_pass.set_pipeline(&self.pipelines.shadow);
            shadow_pass.set_bind_group(0, &bindings.camera, &[]);
            shadow_pass.set_bind_group(1, &bindings.draws, &[]);
            encode_draw_calls(&mut shadow_pass, inputs.draw_calls);
        }
        if let (Some(bindings), Some(occlusion)) = (inputs.bindings, inputs.occlusion) {
            self.encode_occlusion_passes(
                encoder,
                bindings,
                occlusion,
                inputs.draw_calls,
                inputs.targets.depth_test,
                inputs.timestamp_query_set,
            );
        }
        // With a prepass, depth is already written and the timestamps began there.
        let prepassed = inputs.occlusion.is_some();
        let background = wgpu::Color {
            r: f64::from(inputs.clear[0]),
            g: f64::from(inputs.clear[1]),
            b: f64::from(inputs.clear[2]),
            a: f64::from(inputs.clear[3]),
        };
        let color_attachments = main_color_attachments(inputs, background);
        let last_pass = !self.config.rgb_fxaa() && inputs.reflection.is_none();
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("bop scene pass"),
            color_attachments: &color_attachments,
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: inputs.targets.depth_test,
                depth_ops: Some(wgpu::Operations {
                    load: if prepassed {
                        wgpu::LoadOp::Load
                    } else {
                        wgpu::LoadOp::Clear(1.0)
                    },
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            // GPU timing runs from the frame's first pass (the prepass when
            // there is one) to its last (reflections, then FXAA, follow this
            // pass when they run).
            timestamp_writes: inputs
                .timestamp_query_set
                .map(|query_set| wgpu::RenderPassTimestampWrites {
                    query_set,
                    beginning_of_pass_write_index: (!prepassed).then_some(0),
                    end_of_pass_write_index: last_pass.then_some(1),
                })
                .filter(|writes| {
                    writes.beginning_of_pass_write_index.is_some()
                        || writes.end_of_pass_write_index.is_some()
                }),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if let Some(bindings) = inputs.bindings {
            pass.set_pipeline(&self.pipelines.scene);
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
            encode_material_draw_calls(&mut pass, inputs.draw_calls, &self.white_material);
        }
        drop(pass);
        if let Some(reflection) = inputs.reflection {
            self.encode_post_passes(encoder, reflection, inputs, background);
        }
    }

    /// The lit scene's mip chain, then reflections composited into the sRGB target.
    fn encode_post_passes(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        reflection: &ReflectionResources,
        inputs: &ScenePassInputs<'_>,
        background: wgpu::Color,
    ) {
        for (target, source) in &reflection.mip_levels {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bop lit scene downsample"),
                color_attachments: &[Some(color_attachment(target, wgpu::Color::BLACK))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.post.downsample);
            pass.set_bind_group(0, source, &[]);
            pass.draw(0..3, 0..1);
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("bop reflections and composite"),
            color_attachments: &[Some(color_attachment(inputs.targets.rgb, background))],
            depth_stencil_attachment: None,
            timestamp_writes: inputs
                .timestamp_query_set
                .filter(|_| !self.config.rgb_fxaa())
                .map(|query_set| wgpu::RenderPassTimestampWrites {
                    query_set,
                    beginning_of_pass_write_index: None,
                    end_of_pass_write_index: Some(1),
                }),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.post.reflection);
        pass.set_bind_group(0, &reflection.group, &[]);
        pass.draw(0..3, 0..1);
    }

    /// Prepass, occlusion and blur: leaves depth in `depth_test` and the
    /// blurred occlusion in `occlusion.blurred_view`.
    fn encode_occlusion_passes(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bindings: &SceneBindings,
        occlusion: &OcclusionResources,
        draw_calls: &[DrawCall<'_>],
        depth_test: &wgpu::TextureView,
        timestamp_query_set: Option<&wgpu::QuerySet>,
    ) {
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("bop depth and normal prepass"),
                color_attachments: &[
                    Some(color_attachment(
                        &occlusion.normal_view,
                        wgpu::Color::TRANSPARENT,
                    )),
                    Some(color_attachment(
                        &occlusion.camera_z_view,
                        wgpu::Color::TRANSPARENT,
                    )),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_test,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: timestamp_query_set.map(|query_set| {
                    wgpu::RenderPassTimestampWrites {
                        query_set,
                        beginning_of_pass_write_index: Some(0),
                        end_of_pass_write_index: None,
                    }
                }),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipelines.prepass);
            pass.set_bind_group(0, &bindings.camera, &[]);
            pass.set_bind_group(1, &bindings.draws, &[]);
            encode_draw_calls(&mut pass, draw_calls);
        }
        for (label, pipeline, group, target) in [
            (
                "bop ambient occlusion",
                &self.occlusion.occlusion,
                &occlusion.occlusion_group,
                &occlusion.raw_view,
            ),
            (
                "bop ambient occlusion blur",
                &self.occlusion.blur,
                &occlusion.blur_group,
                &occlusion.blurred_view,
            ),
        ] {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(color_attachment(target, wgpu::Color::WHITE))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    fn create_occlusion_resources(
        &self,
        extent: wgpu::Extent3d,
        bindings: &SceneBindings,
    ) -> OcclusionResources {
        let view = |format, label| {
            create_sampled_target(&self.device, extent, format, label)
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let normal_view = view(wgpu::TextureFormat::Rgba16Float, "prepass normals");
        let camera_z_view = view(wgpu::TextureFormat::R32Float, "prepass camera Z");
        let raw_view = view(wgpu::TextureFormat::R16Float, "raw ambient occlusion");
        let blurred_view = view(wgpu::TextureFormat::R16Float, "ambient occlusion");
        let texture = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let occlusion_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bop occlusion bind group"),
            layout: &self.occlusion.occlusion_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: bindings.camera_buffer.as_entire_binding(),
                },
                texture(1, &normal_view),
                texture(2, &camera_z_view),
            ],
        });
        let blur_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bop occlusion blur bind group"),
            layout: &self.occlusion.blur_layout,
            entries: &[texture(2, &camera_z_view), texture(3, &raw_view)],
        });
        OcclusionResources {
            normal_view,
            camera_z_view,
            raw_view,
            blurred_view,
            occlusion_group,
            blur_group,
        }
    }

    /// The shadow map, plus the frame's ambient occlusion that the main pass
    /// reads from the same bind group.
    fn create_shadow_resources(&self, occlusion: &wgpu::TextureView) -> ShadowResources {
        let size = self.config.shadow_map_size();
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bop directional shadow map"),
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
            label: Some("bop shadow comparison sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bop shadow bind group"),
            layout: &self.pipelines.shadow_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(occlusion),
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
        timestamp_query_set: Option<&wgpu::QuerySet>,
    ) {
        let dimensions = [width as f32, height as f32, 0.0, 0.0];
        let dimensions_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("bop FXAA dimensions"),
                contents: bytemuck::cast_slice(&dimensions),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bop FXAA bind group"),
            layout: &self.post.fxaa_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.post.fxaa_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: dimensions_buffer.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("bop RGB FXAA pass"),
            color_attachments: &[Some(color_attachment(target, wgpu::Color::TRANSPARENT))],
            depth_stencil_attachment: None,
            // FXAA is the frame's last pass, so it ends the GPU timing.
            timestamp_writes: timestamp_query_set.map(|query_set| {
                wgpu::RenderPassTimestampWrites {
                    query_set,
                    beginning_of_pass_write_index: None,
                    end_of_pass_write_index: Some(1),
                }
            }),
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.post.fxaa);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    }

    fn begin_camera(
        &self,
        camera: &CameraSnapshot,
        scene: &SceneSnapshot,
        appearance: &AppearanceSnapshot,
        materials: &[bop_core::InstanceMaterial],
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
            let chosen = materials
                .binary_search_by_key(
                    &instance.instance_id(),
                    bop_core::InstanceMaterial::instance_id,
                )
                .ok()
                .map(|index| &materials[index]);
            for primitive in &asset.primitives {
                let draw_index =
                    u32::try_from(uniforms.len()).map_err(|_| WgpuSceneError::DrawCountOverflow)?;
                let (base_color, metallic, roughness) = match chosen {
                    Some(material)
                        if material.scope() == bop_core::MaterialScope::All
                            || !primitive.has_file_material =>
                    {
                        let [red, green, blue, alpha] = material.base_color_srgb().rgba();
                        (
                            [
                                srgb_to_linear(red),
                                srgb_to_linear(green),
                                srgb_to_linear(blue),
                                alpha,
                            ],
                            material.metallic(),
                            material.roughness(),
                        )
                    }
                    _ => (
                        primitive.base_color,
                        primitive.metallic,
                        primitive.roughness,
                    ),
                };
                uniforms.push(DrawUniform {
                    world_from_model: world_from_model.to_cols_array_2d(),
                    normal_from_model: rotation.to_cols_array_2d(),
                    base_color,
                    material: [metallic, roughness, 0.0, 0.0],
                    instance_id: u32::from(instance.instance_id().get()),
                    padding: [0; 3],
                });
                calls.push(DrawCall {
                    primitive,
                    draw_index,
                });
            }
        }
        self.push_bin_draws(scene, &mut uniforms, &mut calls)?;
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
            self.config,
            &self.environment.uniform(appearance.environment())?,
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

    /// Draws the five bin cuboids physics collided with as background
    /// (instance 0): they occlude and write depth but never label pixels.
    fn push_bin_draws<'a>(
        &'a self,
        scene: &SceneSnapshot,
        uniforms: &mut Vec<DrawUniform>,
        calls: &mut Vec<DrawCall<'a>>,
    ) -> Result<(), WgpuSceneError> {
        for bin in scene.bins() {
            let world_from_bin = rigid_matrix(bin.world_from_bin());
            let normal_from_model = rotation_matrix(bin.world_from_bin()).to_cols_array_2d();
            let material = bin.render_material();
            let [red, green, blue, alpha] = material.base_color_srgb().rgba();
            let base_color = [
                srgb_to_linear(red),
                srgb_to_linear(green),
                srgb_to_linear(blue),
                alpha,
            ];
            let cuboids = analytic_bin_snapshot(bin)
                .map_err(|_| WgpuSceneError::BinGeometry)?
                .cuboids();
            for cuboid in cuboids {
                let draw_index =
                    u32::try_from(uniforms.len()).map_err(|_| WgpuSceneError::DrawCountOverflow)?;
                let half_extents = cuboid.half_extents_m().map(bop_core::PositiveMeters::get);
                let world_from_model = world_from_bin
                    * rigid_matrix(cuboid.bin_from_cuboid())
                    * glam::Mat4::from_scale(glam::Vec3::from_array(half_extents));
                uniforms.push(DrawUniform {
                    world_from_model: world_from_model.to_cols_array_2d(),
                    normal_from_model,
                    base_color,
                    material: [material.metallic(), material.roughness(), 0.0, 0.0],
                    instance_id: 0,
                    padding: [0; 3],
                });
                calls.push(DrawCall {
                    primitive: &self.unit_cube,
                    draw_index,
                });
            }
        }
        Ok(())
    }

    fn begin_request_camera(
        &self,
        camera: &CameraSnapshot,
        request: &RenderSceneRequest,
    ) -> Result<PendingReadback, WgpuSceneError> {
        self.begin_camera(
            camera,
            request.scene(),
            request.appearance(),
            request.instance_materials(),
        )
    }

    #[cfg(test)]
    fn render_camera(
        &self,
        camera: &CameraSnapshot,
        scene: &SceneSnapshot,
        appearance: &AppearanceSnapshot,
    ) -> Result<ReadbackFrame, WgpuSceneError> {
        self.begin_camera(camera, scene, appearance, &[])?
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
                .begin_request_camera(camera, request)
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
            encode_frame(staging, request, camera, frame)?;
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

/// Encodes one camera's rendered channels into the staging area. The channels
/// are separate files, so they are encoded concurrently; errors are still
/// reported in the order RGB, depth, instance.
fn encode_frame(
    staging: &SceneStagingArea,
    request: &RenderSceneRequest,
    camera: CameraSnapshot,
    frame: ReadbackFrame,
) -> Result<(), RenderError> {
    let size = camera.image_size();
    let channels = request.required_channels();
    let (rgb, depth, instance) = std::thread::scope(|scope| {
        let depth = scope.spawn(|| {
            channels.depth().then(|| {
                DepthFrame::try_new(size.width(), size.height(), frame.depth)
                    .map_err(|error| invalid_frame("depth", error))?
                    .write_exr(&staging.depth_path(camera.camera_id()))
                    .map_err(|error| render_encoding("depth EXR", &error))
            })
        });
        let instance = scope.spawn(|| {
            channels.instance().then(|| {
                InstanceFrame::try_new(size.width(), size.height(), frame.instance)
                    .map_err(|error| invalid_frame("instance", error))?
                    .write_png(
                        &staging.instance_path(camera.camera_id()),
                        request.png_compression(),
                    )
                    .map_err(|error| render_encoding("instance PNG", &error))
            })
        });
        let rgb = channels.rgb().then(|| {
            let rgb = frame
                .rgba8
                .chunks_exact(4)
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
                .collect();
            RgbFrame::try_new(size.width(), size.height(), rgb)
                .map_err(|error| invalid_frame("RGB", error))?
                .write_png(
                    &staging.rgb_path(camera.camera_id()),
                    request.png_compression(),
                )
                .map_err(|error| render_encoding("RGB PNG", &error))
        });
        (rgb, depth.join(), instance.join())
    });
    let panicked = |channel: &str| RenderError::Internal {
        message: format!("the {channel} encoder panicked"),
    };
    rgb.transpose()?;
    depth.map_err(|_| panicked("depth"))?.transpose()?;
    instance.map_err(|_| panicked("instance"))?.transpose()?;
    Ok(())
}

fn render_io(operation: &str, error: &std::io::Error) -> RenderError {
    RenderError::Io {
        operation: operation.to_owned(),
        message: error.to_string(),
    }
}

fn render_encoding(operation: &str, error: &bop_dataset::FrameWriteError) -> RenderError {
    RenderError::Io {
        operation: operation.to_owned(),
        message: error.to_string(),
    }
}

fn invalid_frame(channel: &str, error: bop_dataset::FrameValidationError) -> RenderError {
    RenderError::Internal {
        message: format!("GPU {channel} readback violated frame invariants: {error}"),
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

/// What orders adapters: rank, then backend, then stable adapter metadata.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct AdapterKey {
    rank: u8,
    backend_rank: u8,
    vendor: u32,
    device: u32,
    name: String,
    backend: String,
}

/// Picks the adapter for `worker_index` from `keys`, in enumeration order.
///
/// The best adapter and every adapter tied with it on rank and backend form
/// the worker's choices; identical GPUs have identical keys, so enumeration
/// order separates them.
fn worker_adapter(keys: &[AdapterKey], worker_index: u32) -> Option<usize> {
    let best = keys.iter().min()?;
    let mut peers: Vec<usize> = (0..keys.len())
        .filter(|&index| keys[index].rank == best.rank && keys[index].backend == best.backend)
        .collect();
    peers.sort_by(|&left, &right| keys[left].cmp(&keys[right]));
    let slot = usize::try_from(worker_index).ok()? % peers.len();
    Some(peers[slot])
}

/// Native backends before OpenGL, which can also expose the same GPU.
const fn backend_rank(backend: wgpu::Backend) -> u8 {
    match backend {
        wgpu::Backend::Vulkan | wgpu::Backend::Metal | wgpu::Backend::Dx12 => 0,
        wgpu::Backend::Gl => 1,
        _ => 2,
    }
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

fn validate_buffer_size(
    asset_id: &bop_core::AssetId,
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

    use bop_assets::{
        ConvexPart, NormalizedAsset, ResolvedAsset, ResolvedAssetCatalog, VisualMesh,
        VisualPrimitive,
    };
    use bop_core::{AssetId, RenderSceneRequest, WgpuAdapterPreference, WgpuDevicePreference};
    use bop_render_api::Renderer;

    use super::{
        AdapterKey, MappingCompletionError, adapter_rank, directional_shadow_matrix, pack_vertices,
        receive_mapping, worker_adapter,
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

    fn key(rank: u8, name: &str, backend: &str) -> AdapterKey {
        AdapterKey {
            rank,
            backend_rank: u8::from(backend == "Gl"),
            vendor: 0x10de,
            device: 0x1db4,
            name: name.to_owned(),
            backend: backend.to_owned(),
        }
    }

    #[test]
    fn workers_spread_over_adapters_tied_with_the_best() {
        let keys = [
            key(1, "integrated", "Vulkan"),
            key(0, "V100", "Vulkan"),
            key(0, "V100", "Gl"),
            key(0, "V100", "Vulkan"),
        ];
        let chosen: Vec<_> = (0..4)
            .map(|worker| worker_adapter(&keys, worker).unwrap())
            .collect();
        // Both Vulkan V100s in enumeration order; never GL or the iGPU.
        assert_eq!(chosen, [1, 3, 1, 3]);
        assert_eq!(worker_adapter(&keys[..1], 5), Some(0));
        assert_eq!(worker_adapter(&[], 0), None);
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
        let packed = pack_vertices(&without_uv, 0);
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
        assert_float_array(pack_vertices(&with_uv, 0)[0].uv, [0.25, 0.75]);
    }

    #[test]
    #[ignore = "manual hardware-dependent offscreen readback smoke test"]
    fn offscreen_clear_targets_round_trip_all_canonical_channels() {
        let config = bop_core::WgpuRendererConfig::try_new(
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
        let config = bop_core::WgpuRendererConfig::try_new(
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
    #[ignore = "manual hardware-dependent bin render/readback test"]
    fn empty_bin_renders_as_background_with_floor_depth_and_its_material() {
        const REQUEST: &str = r#"{"schema_version":1,"scene":{"schema_version":1,"scene_id":1,"accepted_attempt":0,"seed_fingerprint":"seed","bin":{"schema_version":1,"inner_dimensions_m":[1.0,1.0,0.3],"floor_thickness_m":0.1,"wall_thickness_m":0.1,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel","render_material":{"base_color_srgb":[1.0,0.0,0.0,1.0],"metallic":0.0,"roughness":1.0}},"instances":[],"physics":{"schema_version":1,"attempts":1,"steps":1,"simulated_seconds":0.01,"final_max_linear_speed_mps":0.0,"final_max_angular_speed_rad_s":0.0,"final_max_penetration_m":0.0,"rejection_counts":[]}},"cameras":[{"schema_version":1,"camera_id":0,"image_size":{"width":32,"height":32},"k":[25.0,0.0,16.0,0.0,25.0,16.0,0.0,0.0,1.0],"camera_from_world":{"camera_from_world":{"translation":[0.0,0.0,2.0],"rotation_xyzw":[1.0,0.0,0.0,0.0]}},"near_m":0.1,"far_m":10.0}],"appearance":{"schema_version":1,"background_srgb":[0.0,0.0,0.0,1.0],"ambient_light":{"schema_version":1,"color":[1.0,1.0,1.0],"intensity":1.0},"directional_lights":[],"point_lights":[]},"required_channels":{"schema_version":1,"rgb":true,"depth":true,"instance":true}}"#;
        let request: RenderSceneRequest = serde_json::from_str(REQUEST).unwrap();
        let config = bop_core::WgpuRendererConfig::try_new(
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

        // The camera looks straight down from 2 m onto the floor surface at z = 0.
        let center = 16 * 32 + 16;
        assert_eq!(frame.instance[center], 0);
        assert!((frame.depth[center] - 2.0).abs() <= 1.0e-4);
        let [red, green, blue, _] = frame.rgba8[center * 4..center * 4 + 4] else {
            unreachable!()
        };
        // The red bin material, plus the uncoloured few percent a dielectric
        // reflects of the white environment.
        assert!(red > 4 * green && green == blue, "{red} {green} {blue}");
        assert!(frame.instance.iter().all(|id| *id == 0));
    }

    /// The example cube's texture has four quadrants (white with an F, blue,
    /// green, yellow) and a [1, 0.6, 0.6] factor. Seen from straight above,
    /// its top face shows the texture upright, tinted by the factor.
    #[test]
    #[ignore = "manual hardware-dependent test"]
    fn textured_cube_shows_its_texture_upright_and_tinted_by_the_factor() {
        // A camera 2 m up looking down: image right is +x, image down is -y.
        const REQUEST: &str = r#"{"schema_version":1,"scene":{"schema_version":1,"scene_id":1,"accepted_attempt":0,"seed_fingerprint":"seed","bin":{"schema_version":1,"inner_dimensions_m":[1.0,1.0,0.3],"floor_thickness_m":0.1,"wall_thickness_m":0.1,"world_from_bin":{"translation":[0.0,0.0,0.0],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"material_label":"steel"},"instances":[{"schema_version":1,"instance_id":1,"asset_id":"cube","world_from_model":{"translation":[0.0,0.0,0.1],"rotation_xyzw":[0.0,0.0,0.0,1.0]},"physical_properties":{"schema_version":1,"mass_kg":1.0,"friction":0.5,"restitution":0.0}}],"physics":{"schema_version":1,"attempts":1,"steps":1,"simulated_seconds":0.01,"final_max_linear_speed_mps":0.0,"final_max_angular_speed_rad_s":0.0,"final_max_penetration_m":0.0,"rejection_counts":[]}},"cameras":[{"schema_version":1,"camera_id":0,"image_size":{"width":64,"height":64},"k":[1400.0,0.0,32.0,0.0,1400.0,32.0,0.0,0.0,1.0],"camera_from_world":{"camera_from_world":{"translation":[0.0,0.0,2.0],"rotation_xyzw":[1.0,0.0,0.0,0.0]}},"near_m":0.1,"far_m":10.0}],"appearance":{"schema_version":1,"background_srgb":[0.0,0.0,0.0,1.0],"ambient_light":{"schema_version":1,"color":[1.0,1.0,1.0],"intensity":1.0},"directional_lights":[],"point_lights":[]},"required_channels":{"schema_version":1,"rgb":true,"depth":true,"instance":true}}"#;
        let glb = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/assets/textured_cube.glb"
        ))
        .unwrap();
        let limits =
            bop_assets::GlbLoadLimits::try_new(8, 8, 1024, 1024, 256, 1 << 20, 1 << 22).unwrap();
        let loaded = bop_assets::load_glb(&glb, limits).unwrap();
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
                loaded.visual_mesh().clone(),
                loaded.materials().to_vec(),
                loaded.textures().to_vec(),
                vec![collision],
            )
            .unwrap(),
        );
        let catalog = ResolvedAssetCatalog::try_new(vec![ResolvedAsset::new(
            AssetId::new("cube").unwrap(),
            *blake3::hash(b"textured-cube").as_bytes(),
            asset,
        )])
        .unwrap();
        let request: RenderSceneRequest = serde_json::from_str(REQUEST).unwrap();
        let config = bop_core::WgpuRendererConfig::try_new(
            WgpuDevicePreference::Automatic,
            WgpuAdapterPreference::HighPerformance,
            64,
            false,
        )
        .unwrap();
        let mut renderer = super::WgpuRenderer::try_new(config).unwrap();
        renderer.prepare(&catalog).unwrap();
        let frame = renderer
            .render_camera(&request.cameras()[0], request.scene(), request.appearance())
            .unwrap();
        let pixel = |x: usize, y: usize| {
            let i = (y * 64 + x) * 4;
            [frame.rgba8[i], frame.rgba8[i + 1], frame.rgba8[i + 2]].map(i32::from)
        };
        // The top face spans about 60 pixels; sample each quadrant's centre
        // region, clear of the F's strokes.
        let [r, g, b] = pixel(23, 23);
        assert!(
            r > g + 40 && (g - b).abs() < 8,
            "top left, white tinted pink: {r} {g} {b}"
        );
        let [r, g, b] = pixel(41, 23);
        assert!(b > r + 60 && b > g + 30, "top right, blue: {r} {g} {b}");
        let [r, g, b] = pixel(23, 41);
        assert!(g > r + 40 && g > b + 40, "bottom left, green: {r} {g} {b}");
        let [r, g, b] = pixel(41, 41);
        assert!(
            r > g + 40 && g > b + 40,
            "bottom right, yellow tinted orange: {r} {g} {b}"
        );

        // An instance material: `all` retints the texture, `unassigned`
        // leaves the file's material alone.
        let with_material = |scope: &str| {
            let mut json: serde_json::Value = serde_json::from_str(REQUEST).unwrap();
            json["instance_materials"] = serde_json::json!([{
                "instance_id": 1, "scope": scope,
                "base_color_srgb": [0.2, 0.2, 1.0, 1.0], "metallic": 0.0, "roughness": 1.0
            }]);
            let request: RenderSceneRequest = serde_json::from_value(json).unwrap();
            renderer
                .begin_camera(
                    &request.cameras()[0],
                    request.scene(),
                    request.appearance(),
                    request.instance_materials(),
                )
                .unwrap()
                .finish(&renderer.device)
                .unwrap()
        };
        let retinted = with_material("all");
        let i = (23 * 64 + 23) * 4;
        let [r, g, b] = [
            retinted.rgba8[i],
            retinted.rgba8[i + 1],
            retinted.rgba8[i + 2],
        ]
        .map(i32::from);
        assert!(
            b > r + 60 && b > g + 60,
            "top left, white tinted blue: {r} {g} {b}"
        );
        assert_eq!(with_material("unassigned").rgba8, frame.rgba8);
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
        let config = bop_core::WgpuRendererConfig::try_new(
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
            bop_dataset::SceneStagingArea::create(root.path(), request.scene().scene_id()).unwrap();
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
        bop_dataset::validate_and_write_checksums(&staging, &request).unwrap();
        assert!(staging.root().join("checksums.json").is_file());
        for camera in request.cameras() {
            assert!(staging.rgb_path(camera.camera_id()).is_file());
            assert!(staging.depth_path(camera.camera_id()).is_file());
            assert!(staging.instance_path(camera.camera_id()).is_file());
        }

        let failed_staging =
            bop_dataset::SceneStagingArea::create(root.path(), request.scene().scene_id()).unwrap();
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

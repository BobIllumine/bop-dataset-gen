//! Base-color textures: mipmapped upload, glTF samplers, and material bind groups.

#[allow(
    clippy::wildcard_imports,
    reason = "submodules share the crate root's imports and helpers"
)]
use super::*;

use bop_assets::{MagnificationFilter, MinificationFilter, Texture, TextureSampler, WrapMode};

/// sRGB RGBA8 levels from full size down to 1x1, each a 2x2 box filter of the
/// previous one in linear light (alpha is filtered as is).
pub(crate) fn mip_chain(rgba8_srgb: &[u8], width: u32, height: u32) -> Vec<(u32, u32, Vec<u8>)> {
    let to_linear: Vec<f32> = (0..=255_u8)
        .map(|value| srgb_to_linear(f32::from(value) / 255.0))
        .collect();
    let mut levels = vec![(width, height, rgba8_srgb.to_vec())];
    let mut linear: Vec<f32> = rgba8_srgb
        .chunks_exact(4)
        .flat_map(|p| {
            [
                to_linear[p[0] as usize],
                to_linear[p[1] as usize],
                to_linear[p[2] as usize],
                f32::from(p[3]) / 255.0,
            ]
        })
        .collect();
    let (mut w, mut h) = (width, height);
    while w > 1 || h > 1 {
        let (next_w, next_h) = ((w / 2).max(1), (h / 2).max(1));
        let mut next = vec![0.0_f32; (next_w * next_h * 4) as usize];
        for y in 0..next_h {
            for x in 0..next_w {
                for channel in 0..4 {
                    let mut sum = 0.0;
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let sx = (x * 2 + dx).min(w - 1);
                        let sy = (y * 2 + dy).min(h - 1);
                        sum += linear[((sy * w + sx) * 4 + channel) as usize];
                    }
                    next[((y * next_w + x) * 4 + channel) as usize] = sum / 4.0;
                }
            }
        }
        let bytes = next
            .chunks_exact(4)
            .flat_map(|p| {
                [
                    unit_to_byte(linear_to_srgb(p[0])),
                    unit_to_byte(linear_to_srgb(p[1])),
                    unit_to_byte(linear_to_srgb(p[2])),
                    unit_to_byte(p[3]),
                ]
            })
            .collect();
        levels.push((next_w, next_h, bytes));
        linear = next;
        (w, h) = (next_w, next_h);
    }
    levels
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is rounded and clamped to 0..=255 first"
)]
fn unit_to_byte(value: f32) -> u8 {
    (value * 255.0).round().clamp(0.0, 255.0) as u8
}

fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn address_mode(mode: WrapMode) -> wgpu::AddressMode {
    match mode {
        WrapMode::Repeat => wgpu::AddressMode::Repeat,
        WrapMode::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
        WrapMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
    }
}

/// The glTF sampler as a wgpu sampler; unspecified filters are trilinear, and
/// fully linear sampling is also anisotropic.
pub(crate) fn sampler_descriptor(sampler: TextureSampler) -> wgpu::SamplerDescriptor<'static> {
    use wgpu::{FilterMode::Linear, FilterMode::Nearest, MipmapFilterMode};
    let mag_filter = match sampler.magnification() {
        Some(MagnificationFilter::Nearest) => Nearest,
        Some(MagnificationFilter::Linear) | None => Linear,
    };
    let (min_filter, mipmap_filter, mipmapped) = match sampler.minification() {
        Some(MinificationFilter::Nearest) => (Nearest, MipmapFilterMode::Nearest, false),
        Some(MinificationFilter::Linear) => (Linear, MipmapFilterMode::Nearest, false),
        Some(MinificationFilter::NearestMipmapNearest) => {
            (Nearest, MipmapFilterMode::Nearest, true)
        }
        Some(MinificationFilter::LinearMipmapNearest) => (Linear, MipmapFilterMode::Nearest, true),
        Some(MinificationFilter::NearestMipmapLinear) => (Nearest, MipmapFilterMode::Linear, true),
        Some(MinificationFilter::LinearMipmapLinear) | None => {
            (Linear, MipmapFilterMode::Linear, true)
        }
    };
    let trilinear =
        mag_filter == Linear && min_filter == Linear && mipmap_filter == MipmapFilterMode::Linear;
    wgpu::SamplerDescriptor {
        label: Some("bop base-color sampler"),
        address_mode_u: address_mode(sampler.wrap_u()),
        address_mode_v: address_mode(sampler.wrap_v()),
        mag_filter,
        min_filter,
        mipmap_filter,
        // Without mipmapped minification only the full-size level is sampled.
        lod_max_clamp: if mipmapped { 32.0 } else { 0.0 },
        anisotropy_clamp: if trilinear { 8 } else { 1 },
        ..Default::default()
    }
}

pub(crate) fn create_material_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("bop material layout"),
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
        ],
    })
}

/// Uploads one texture with its full mip chain.
pub(crate) fn upload_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &Texture,
) -> wgpu::TextureView {
    let [width, height] = texture.image().dimensions();
    let levels = mip_chain(texture.image().rgba8_srgb(), width, height);
    let gpu = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("bop base-color texture"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: u32::try_from(levels.len()).expect("at most 32 levels"),
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    for (level, (w, h, bytes)) in levels.iter().enumerate() {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &gpu,
                mip_level: u32::try_from(level).expect("at most 32 levels"),
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(*h),
            },
            wgpu::Extent3d {
                width: *w,
                height: *h,
                depth_or_array_layers: 1,
            },
        );
    }
    gpu.create_view(&wgpu::TextureViewDescriptor::default())
}

pub(crate) fn material_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("bop material bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// A 1x1 white texture: untextured materials multiply their factor by one.
pub(crate) fn white_material(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    layout: &wgpu::BindGroupLayout,
) -> wgpu::BindGroup {
    let texture = Texture::new(
        bop_assets::TextureImage::try_new(1, 1, vec![255; 4]).expect("1x1 RGBA8"),
        TextureSampler::new(None, None, WrapMode::Repeat, WrapMode::Repeat),
    );
    let view = upload_texture(device, queue, &texture);
    let sampler = device.create_sampler(&sampler_descriptor(texture.sampler()));
    material_bind_group(device, layout, &view, &sampler)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_chain_halves_to_one_texel_and_filters_in_linear_light() {
        // 4x2: left half black, right half white.
        let mut image = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                image.extend(if x < 2 {
                    [0, 0, 0, 255]
                } else {
                    [255, 255, 255, 255]
                });
            }
        }
        let levels = mip_chain(&image, 4, 2);
        let sizes: Vec<_> = levels.iter().map(|(w, h, _)| (*w, *h)).collect();
        assert_eq!(sizes, [(4, 2), (2, 1), (1, 1)]);
        assert_eq!(levels[1].2, [0, 0, 0, 255, 255, 255, 255, 255]);
        // Half black, half white is 0.5 in linear light, 188 in sRGB (not 128).
        assert_eq!(levels[2].2, [188, 188, 188, 255]);
    }

    #[test]
    fn gltf_samplers_map_to_wgpu_filters_and_wrapping() {
        let default = sampler_descriptor(TextureSampler::new(
            None,
            None,
            WrapMode::Repeat,
            WrapMode::ClampToEdge,
        ));
        assert_eq!(default.mag_filter, wgpu::FilterMode::Linear);
        assert_eq!(default.mipmap_filter, wgpu::MipmapFilterMode::Linear);
        assert_eq!(default.anisotropy_clamp, 8);
        assert_eq!(default.address_mode_u, wgpu::AddressMode::Repeat);
        assert_eq!(default.address_mode_v, wgpu::AddressMode::ClampToEdge);

        let pixelated = sampler_descriptor(TextureSampler::new(
            Some(MagnificationFilter::Nearest),
            Some(MinificationFilter::Nearest),
            WrapMode::MirroredRepeat,
            WrapMode::MirroredRepeat,
        ));
        assert_eq!(pixelated.mag_filter, wgpu::FilterMode::Nearest);
        assert_eq!(pixelated.lod_max_clamp.to_bits(), 0.0_f32.to_bits());
        assert_eq!(pixelated.anisotropy_clamp, 1);
        assert_eq!(pixelated.address_mode_u, wgpu::AddressMode::MirrorRepeat);
    }
}

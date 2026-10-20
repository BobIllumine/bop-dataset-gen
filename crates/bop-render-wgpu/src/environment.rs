//! Image-based lighting: GGX-prefiltered radiance levels and diffuse
//! spherical harmonics from an equirectangular environment map.

use std::f32::consts::PI;

use glam::Vec3;

/// Roughness levels stored as mips: level `k` is roughness `k / (LEVELS - 1)`.
pub(crate) const LEVELS: u32 = 6;
/// Widest prefiltered level; larger maps are box-filtered down first.
const MAX_WIDTH: usize = 1024;
/// GGX samples per texel for rough levels.
const SAMPLES: u32 = 96;

/// Linear RGB equirectangular image, rows from the zenith down.
#[derive(Clone)]
pub(crate) struct Equirect {
    pub(crate) width: usize,
    pub(crate) height: usize,
    pub(crate) rgb: Vec<Vec3>,
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "texel indices and counts are at most 16384, exact in f32"
)]
impl Equirect {
    fn downsampled(&self) -> Self {
        let (width, height) = ((self.width / 2).max(2), (self.height / 2).max(1));
        let mut rgb = vec![Vec3::ZERO; width * height];
        for y in 0..height {
            for x in 0..width {
                let mut sum = Vec3::ZERO;
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let sx = (x * 2 + dx).min(self.width - 1);
                    let sy = (y * 2 + dy).min(self.height - 1);
                    sum += self.rgb[sy * self.width + sx];
                }
                rgb[y * width + x] = sum / 4.0;
            }
        }
        Self { width, height, rgb }
    }

    /// Bilinear radiance in direction `d` (wrapping in longitude).
    #[allow(
        clippy::many_single_char_names,
        reason = "conventional texture-coordinate names"
    )]
    fn sample(&self, d: Vec3) -> Vec3 {
        let [u, v] = direction_to_uv(d);
        let x = u * self.width as f32 - 0.5;
        let y = (v * self.height as f32 - 0.5).clamp(0.0, self.height as f32 - 1.0);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let column = |c: f32| (c as i64).rem_euclid(self.width as i64) as usize;
        let row = |r: f32| (r as usize).min(self.height - 1);
        let at = |c: f32, r: f32| self.rgb[row(r) * self.width + column(c)];
        let top = at(x0, y0).lerp(at(x0 + 1.0, y0), fx);
        let bottom = at(x0, y0 + 1.0).lerp(at(x0 + 1.0, y0 + 1.0), fx);
        top.lerp(bottom, fy)
    }
}

/// Blender's equirectangular mapping: the image centre faces world +X, +Y
/// is a quarter turn to the left of it, rows run from the zenith down.
pub(crate) fn direction_to_uv(d: Vec3) -> [f32; 2] {
    let d = d.normalize_or_zero();
    let u = 0.5 - d.y.atan2(d.x) / (2.0 * PI);
    let v = d.z.clamp(-1.0, 1.0).acos() / PI;
    [u.rem_euclid(1.0), v]
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "texel indices and counts are at most 16384, exact in f32"
)]
fn uv_to_direction(u: f32, v: f32) -> Vec3 {
    let phi = (0.5 - u) * 2.0 * PI;
    let theta = v * PI;
    Vec3::new(
        theta.sin() * phi.cos(),
        theta.sin() * phi.sin(),
        theta.cos(),
    )
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "texel indices and counts are at most 16384, exact in f32"
)]
fn hammersley(i: u32, n: u32) -> [f32; 2] {
    [
        (i as f32 + 0.5) / n as f32,
        i.reverse_bits() as f32 * (1.0 / 4_294_967_296.0),
    ]
}

/// Prefiltered radiance levels (largest first, each half the previous) as
/// RGBA f32, and diffuse irradiance spherical harmonics scaled so a white
/// diffuse surface reflects `sh` evaluated at its normal.
pub(crate) struct Prefiltered {
    pub(crate) levels: Vec<(usize, usize, Vec<[f32; 4]>)>,
    pub(crate) sh: [Vec3; 9],
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "texel indices and counts are at most 16384, exact in f32"
)]
pub(crate) fn prefilter(source: &Equirect) -> Prefiltered {
    let mut base = source.clone();
    while base.width > MAX_WIDTH {
        base = base.downsampled();
    }
    // Source pyramid for filtered importance sampling.
    let mut pyramid = vec![base.clone()];
    while pyramid.last().expect("nonempty").height > 1 {
        let next = pyramid.last().expect("nonempty").downsampled();
        pyramid.push(next);
    }
    let texel_solid_angle = 4.0 * PI / (base.width * base.height) as f32;
    let mut levels = vec![(
        base.width,
        base.height,
        base.rgb.iter().map(|c| [c.x, c.y, c.z, 1.0]).collect(),
    )];
    for level in 1..LEVELS {
        let roughness = level as f32 / (LEVELS - 1) as f32;
        let alpha = roughness * roughness;
        let (width, height) = ((base.width >> level).max(1), (base.height >> level).max(1));
        let mut texels = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let n = uv_to_direction(
                    (x as f32 + 0.5) / width as f32,
                    (y as f32 + 0.5) / height as f32,
                );
                texels.push(prefilter_texel(n, alpha, &pyramid, texel_solid_angle));
            }
        }
        levels.push((width, height, texels));
    }
    Prefiltered {
        levels,
        sh: irradiance_sh(pyramid.iter().find(|p| p.width <= 256).unwrap_or(&base)),
    }
}

/// Split-sum prefiltering with N = V = R (Karis 2013), sampling the pyramid
/// level whose texels match each sample's solid angle.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "texel indices and counts are at most 16384, exact in f32"
)]
fn prefilter_texel(n: Vec3, alpha: f32, pyramid: &[Equirect], texel_solid_angle: f32) -> [f32; 4] {
    let helper = if n.z.abs() < 0.999 { Vec3::Z } else { Vec3::X };
    let tangent = helper.cross(n).normalize();
    let bitangent = n.cross(tangent);
    let alpha2 = alpha * alpha;
    let mut sum = Vec3::ZERO;
    let mut weight = 0.0;
    for i in 0..SAMPLES {
        let [xi1, xi2] = hammersley(i, SAMPLES);
        let phi = 2.0 * PI * xi1;
        let cos_theta = ((1.0 - xi2) / (1.0 + (alpha2 - 1.0) * xi2)).sqrt();
        let sin_theta = (1.0 - cos_theta * cos_theta).sqrt();
        let h =
            tangent * (sin_theta * phi.cos()) + bitangent * (sin_theta * phi.sin()) + n * cos_theta;
        let l = 2.0 * n.dot(h) * h - n;
        let n_dot_l = n.dot(l);
        if n_dot_l <= 0.0 {
            continue;
        }
        let n_dot_h = n.dot(h).max(0.0);
        let d = alpha2 / (PI * ((n_dot_h * n_dot_h * (alpha2 - 1.0) + 1.0).powi(2)));
        let pdf = d / 4.0; // n_dot_h / (4 h_dot_v) with N = V
        let sample_solid_angle = 1.0 / (SAMPLES as f32 * pdf + 1e-6);
        let lod = (0.5 * (sample_solid_angle / texel_solid_angle).log2() + 1.0).max(0.0);
        let level = (lod.round() as usize).min(pyramid.len() - 1);
        sum += pyramid[level].sample(l) * n_dot_l;
        weight += n_dot_l;
    }
    let c = sum / weight.max(1e-6);
    [c.x, c.y, c.z, 1.0]
}

/// Order-2 spherical harmonics of the cosine-convolved environment, divided
/// by pi: evaluated at a normal, the radiance a white diffuse surface reflects.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "texel indices and counts are at most 16384, exact in f32"
)]
fn irradiance_sh(image: &Equirect) -> [Vec3; 9] {
    let mut sh = [Vec3::ZERO; 9];
    for y in 0..image.height {
        let v = (y as f32 + 0.5) / image.height as f32;
        let solid_angle =
            (2.0 * PI / image.width as f32) * (PI / image.height as f32) * (v * PI).sin();
        for x in 0..image.width {
            let d = uv_to_direction((x as f32 + 0.5) / image.width as f32, v);
            let c = image.rgb[y * image.width + x] * solid_angle;
            for (coefficient, basis) in sh.iter_mut().zip(sh_basis(d)) {
                *coefficient += c * basis;
            }
        }
    }
    // Cosine-lobe convolution (Ramamoorthi & Hanrahan), then / pi.
    let band = [PI, 2.0 * PI / 3.0, PI / 4.0];
    for (index, coefficient) in sh.iter_mut().enumerate() {
        let l = match index {
            0 => 0,
            1..=3 => 1,
            _ => 2,
        };
        *coefficient *= band[l] / PI;
    }
    sh
}

pub(crate) fn sh_basis(d: Vec3) -> [f32; 9] {
    [
        0.282_095,
        0.488_603 * d.y,
        0.488_603 * d.z,
        0.488_603 * d.x,
        1.092_548 * d.x * d.y,
        1.092_548 * d.y * d.z,
        0.315_392 * (3.0 * d.z * d.z - 1.0),
        1.092_548 * d.x * d.z,
        0.546_274 * (d.x * d.x - d.y * d.y),
    ]
}

/// The prefiltered environment on the GPU, or a black stand-in when the
/// catalog has none.
pub(crate) struct GpuEnvironment {
    pub(crate) view: wgpu::TextureView,
    pub(crate) sampler: wgpu::Sampler,
    sh: [[f32; 4]; 9],
    max_level: f32,
    loaded: bool,
}

/// The environment's part of the camera uniform.
pub(crate) struct EnvironmentUniform {
    pub(crate) environment: [f32; 4],
    pub(crate) levels: [f32; 4],
    pub(crate) sh: [[f32; 4]; 9],
}

impl GpuEnvironment {
    pub(crate) fn empty(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let prefiltered = Prefiltered {
            levels: vec![(1, 1, vec![[0.0; 4]])],
            sh: [Vec3::ZERO; 9],
        };
        Self::upload(device, queue, &prefiltered, false)
    }

    /// The catalog's environment, prefiltered, or the black stand-in.
    pub(crate) fn for_catalog(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        catalog: &bop_assets::ResolvedAssetCatalog,
    ) -> Self {
        catalog.environment().map_or_else(
            || Self::empty(device, queue),
            |map| Self::from_map(device, queue, map),
        )
    }

    fn from_map(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        map: &bop_assets::EnvironmentMap,
    ) -> Self {
        let [width, height] = map.dimensions();
        let source = Equirect {
            width: width as usize,
            height: height as usize,
            rgb: map.rgb().chunks_exact(3).map(Vec3::from_slice).collect(),
        };
        Self::upload(device, queue, &prefilter(&source), true)
    }

    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        reason = "texel indices and counts are at most 16384, exact in f32"
    )]
    fn upload(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        prefiltered: &Prefiltered,
        loaded: bool,
    ) -> Self {
        let (width, height, _) = &prefiltered.levels[0];
        let count = u32::try_from(prefiltered.levels.len()).expect("few levels");
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("bop environment map"),
            size: wgpu::Extent3d {
                width: u32::try_from(*width).expect("bounded width"),
                height: u32::try_from(*height).expect("bounded height"),
                depth_or_array_layers: 1,
            },
            mip_level_count: count,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        for (level, (w, h, texels)) in prefiltered.levels.iter().enumerate() {
            // Half floats top out at 65504; brighter texels (a sun's core) clamp.
            let bytes: Vec<u8> = texels
                .iter()
                .flatten()
                .flat_map(|value| half::f16::from_f32(value.min(65_504.0)).to_le_bytes())
                .collect();
            let (w, h) = (
                u32::try_from(*w).expect("bounded"),
                u32::try_from(*h).expect("bounded"),
            );
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: u32::try_from(level).expect("few levels"),
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 8),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        }
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("bop environment sampler"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        Self {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            sampler,
            sh: prefiltered.sh.map(|c| [c.x, c.y, c.z, 0.0]),
            max_level: (count - 1) as f32,
            loaded,
        }
    }

    /// Uniform values for a scene lit by `light`, or by the ambient light when `None`.
    pub(crate) fn uniform(
        &self,
        light: Option<bop_core::EnvironmentLight>,
    ) -> Result<EnvironmentUniform, crate::WgpuSceneError> {
        let Some(light) = light else {
            return Ok(EnvironmentUniform {
                environment: [0.0; 4],
                levels: [0.0; 4],
                sh: [[0.0; 4]; 9],
            });
        };
        if !self.loaded {
            return Err(crate::WgpuSceneError::MissingEnvironment);
        }
        let (sin, cos) = light.rotation_deg().to_radians().sin_cos();
        Ok(EnvironmentUniform {
            environment: [1.0, light.intensity().get(), cos, sin],
            levels: [self.max_level, 0.0, 0.0, 0.0],
            sh: self.sh,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uniform(radiance: f32) -> Equirect {
        Equirect {
            width: 64,
            height: 32,
            rgb: vec![Vec3::splat(radiance); 64 * 32],
        }
    }

    #[test]
    fn mapping_round_trips_and_faces_plus_x_at_the_centre() {
        assert!((direction_to_uv(Vec3::X)[0] - 0.5).abs() < 1e-6);
        assert!((direction_to_uv(Vec3::Y)[0] - 0.25).abs() < 1e-6);
        assert!(direction_to_uv(Vec3::Z)[1].abs() < 1e-6);
        for (u, v) in [(0.1, 0.2), (0.6, 0.5), (0.9, 0.85)] {
            let [ru, rv] = direction_to_uv(uv_to_direction(u, v));
            assert!((ru - u).abs() < 1e-5 && (rv - v).abs() < 1e-5);
        }
    }

    #[test]
    fn a_uniform_environment_stays_uniform_at_every_roughness() {
        let prefiltered = prefilter(&uniform(2.0));
        for (_, _, texels) in &prefiltered.levels {
            assert!(
                texels.iter().all(|t| (t[0] - 2.0).abs() < 1e-3),
                "{:?}",
                texels[0]
            );
        }
        // A white diffuse surface under uniform radiance L reflects L.
        for normal in [Vec3::Z, Vec3::X, Vec3::new(0.3, -0.5, 0.8).normalize()] {
            let reflected: f32 = prefiltered
                .sh
                .iter()
                .zip(sh_basis(normal))
                .map(|(c, b)| c.x * b)
                .sum();
            assert!((reflected - 2.0).abs() < 0.03, "{reflected}");
        }
    }
}

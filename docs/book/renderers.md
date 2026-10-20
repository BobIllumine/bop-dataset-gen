# Renderer architecture and operation

Both renderers implement `bop_render_api::Renderer` with the same
`ResolvedAssetCatalog`, `RenderSceneRequest`, and `SceneStagingArea` boundary.
There is no implicit fallback: selecting BlenderProc and failing to start it is
an error, not permission to produce a wgpu dataset.

## Coordinate and output contract

Scene geometry is metres in the canonical right-handed Z-up world. Camera
snapshots store OpenCV camera-from-world extrinsics: +X right, +Y down, +Z
forward. RGB is 8-bit sRGB PNG, depth is finite nonnegative camera-Z metres in
a single-channel `Z` float EXR, and visible instance labels are lossless
16-bit grayscale PNG with zero reserved for background.

Both backends draw the bin as the same five cuboids (floor and four walls)
that physics collides with, using `[bin] render_material`. The bin is
background: it occludes objects and writes depth, but its pixels are labelled
zero, so visible masks and BOP `mask_visib` exclude what the walls hide.

Before publication, `bop-dataset` decodes every requested frame and checks its
dimensions and representation. It rejects negative/non-finite depth and labels
not declared by the scene snapshot. It then writes `checksums.json`, containing
sorted BLAKE3 checksums for scene metadata, camera metadata, and every channel.
Only after those checks does the staging directory become a committed scene.

## wgpu backend

`WgpuRenderer` owns one persistent headless instance, adapter, device, queue,
pipeline, and uploaded asset catalog. Adapter selection is deterministic within
the requested CPU/GPU and power class, preferring Vulkan, Metal, and DirectX 12
over OpenGL. Explicit classes are hard requirements. With several renderer
workers, the workers spread over equivalent GPUs; see
[Multiple GPUs](#multiple-gpus).
The render pass writes aligned `Rgba8UnormSrgb`, `R32Float`, and `R32Uint`
targets with a `Depth32Float` visibility buffer. Instance values are checked
before conversion to the canonical `u16` frame. Each camera's three channel
buffers are mapped together, and a fixed three-frame readback ring submits
later cameras before waiting on the oldest frame. This bounds GPU staging
memory while overlapping rendering and mapping across cameras. Row padding is
removed on the CPU before encoding. Buffer-map failures, dropped callbacks,
poll failures, and device-loss callbacks become render errors before any frame
is published; abandoned staging directories remain removable by the dataset
layer.

When `rgb_fxaa` is enabled, the scene color target is filtered by a dedicated
fullscreen FXAA pass before readback. Metric depth and integer instance labels
never enter that pass and remain single-sampled, preserving their exact
geometric and categorical contracts.

Each frame is drawn as: the shadow map, a depth and normal prepass, ambient
occlusion and its blur, shading, a mip chain of the shaded image, reflections
and compositing, and FXAA when enabled.

- **Shading** is the metallic-roughness model Cycles' Principled BSDF uses for
  direct light: GGX distribution, height-correlated Smith visibility and
  Schlick Fresnel, for up to eight directional and eight inverse-square point
  lights. Indirect light comes from the environment: the uniform ambient
  light, or an [environment map](configuration.md#environment-maps). The map
  is prefiltered once per catalog, by GGX importance sampling into one mip
  level per roughness and into spherical harmonics for diffuse light, so
  shading costs a few texture reads.
- **Shadows.** The first directional light owns one orthographic shadow map
  fitted to the scene's bounds, sampled with a 5x5 percentage-closer filter
  from a point offset along the normal. All faces cast shadows, so light does
  not leak where objects meet the floor or walls. Other lights cast none.
- **Ambient occlusion** follows GTAO (Jimenez et al. 2016): from the
  prepass's metric depth, each pixel finds the occluding horizon in four
  screen-space directions within `ambient_occlusion_radius_m` and integrates
  the visible, cosine-weighted part of the sky, which scales the ambient
  light. About the bin's size (default 0.7 m) captures the walls hiding the
  sky as well as contacts between parts; 0 disables it. A 4x4 ordered
  pattern rotates the directions and the blur averages it out, so frames are
  deterministic and free of noise. Occluded diffuse light is then raised by
  the multi-bounce fit of the same paper, standing in for light bouncing
  between bright neighbouring surfaces.
- **Reflections.** Each glossy pixel marches its GGX lobe's dominant
  direction through the prepass depth (`screen_space_reflections`, on by
  default). A hit reflects what the camera sees there, read from the mip level
  matching the lobe's footprint, so a bin wall or a neighbouring part shows
  up, blurred on rougher surfaces. Misses, surfaces rougher than 0.6, and
  everything outside the image reflect the environment instead, dimmed by
  ambient occlusion. Fixed step positions keep frames deterministic.
- **Base-color textures** multiply the base-color factor, as glTF specifies.
  They are uploaded with mipmaps filtered in linear light and sampled as
  their glTF sampler says; fully linear sampling is also 8x anisotropic.

Cross-GPU pixel identity is not claimed. RGB is intended for fast
deterministic rasterized training imagery; depth, labels, geometry, and
metadata are the strict contracts. Light bouncing between surfaces is only
approximated, so colours do not bleed as they do in Cycles, and nothing
outside the image appears in reflections.

On the `small` preset's 100 scenes, wgpu's RGB is 26.5 dB PSNR (mean error
6.4/255) from BlenderProc's with one glossy bounce. Against BlenderProc
without glossy bounces it reached 27.7 dB (4.5/255), up from 22.9 dB
(13.2/255) before physically based shading and ambient occlusion; the glossy
bounce brightens Cycles' darker areas by about 5/255, which multi-bounce
occlusion and reflections now mostly follow. On five two-bin cup and nippel
scenes, ambient occlusion raised agreement from 25.7 to 29.4 dB.

Hardware acceptance on Apple Metal:

```text
cargo test -p bop-render-wgpu --release --locked --lib \
  triangle_render_produces_aligned_rgb_metric_depth_and_instance_pixels \
  -- --ignored --nocapture
```

On the development M4 Pro, the 32x32 lit analytic-triangle render, shadow and
FXAA passes, timestamp resolve, and three-channel readback median was 472.458
microseconds over 31 measured iterations on 2026-08-30. This is a smoke-scale
baseline, not a cross-backend performance claim. The three-camera transactional hardware test also passed
canonical decode, label, depth, checksum, and atomic-publication validation.
When the adapter exposes timestamp queries, reports include `gpu_seconds`
separately from wall-clock render and encoding stages; unsupported adapters
omit that field and advertise `timestamp_queries=false` in backend provenance. The
timing runs from the frame's first pass to its last. On Apple Metal without
`rgb_fxaa`, the end sample is not written in multi-camera scenes, and
`gpu_seconds` is omitted rather than reported as zero.

## BlenderProc backend

`BlenderProcRenderer` starts one persistent external process using:

```text
<executable_path> run <worker_path> -- --bop-worker
```

The worker is [tools/blenderproc-worker/blenderproc_worker.py](../../tools/blenderproc-worker/blenderproc_worker.py).
Worker replies use `BOP1<TAB>` followed by strict protocol-v1 JSON. The Rust
supervisor treats other stdout lines emitted by the BlenderProc launcher as
bounded diagnostics alongside stderr instead of mistaking them for replies.
Startup and requests have independent deadlines, every non-handshake reply must
match its request ID, and an unexpected render-process death triggers exactly
one restart plus catalog preparation replay. Protocol/content failures are not
retried. Drop requests shutdown and then reaps the child defensively.

Preparation transfers checked canonical meshes, materials, and decoded texture
pixels once. Rendering applies Rapier poses directly, configures camera
intrinsics/extrinsics explicitly, and writes temporary little-endian RGB,
float-depth, and instance buffers. Rust validates exact byte counts, converts
them to the same canonical encoders as wgpu, and removes temporary buffers.

The Rust supervisor is fully tested with persistent, malformed-version, and
crash-once fake workers. A real BlenderProc smoke test is capability-gated: it
cannot be claimed on a machine where the `blenderproc` executable and Blender
runtime are absent. Install the configured BlenderProc version, point
`executable_path` and `worker_path` at the runtime and worker, then run the same
scene corpus through both backends into separate dataset roots.

### Installing on Apple Silicon

```text
cd tools/blenderproc-worker && uv sync
.venv/bin/blenderproc run <any script that imports blenderproc>
```

The first `blenderproc run` downloads Blender 4.2.1 LTS, the release
BlenderProc 2.8.0 is pinned to, into `~/blender/` and pip-installs
BlenderProc's packages into that Blender's `custom-python-packages`. It does
not use or modify another Blender installation unless `--custom-blender-path`
is given. Then set `executable_path = "<repo>/tools/blenderproc-worker/.venv/bin/blenderproc"`
(relative paths resolve against the configuration file), as
`examples/cup_nippel/cup_nippel.toml` does.

`device_selection` picks the Cycles device:

- `AUTO` or `GPU` keeps BlenderProc's order: OPTIX, then CUDA, then HIP, or
  METAL on macOS, falling back to the CPU.
- `CPU` renders on the CPU.
- A device type such as `CUDA` requires that type, and fails at the handshake
  if none is present. `CUDA:1` or `OPTIX:0,1` also selects GPU indices, for
  example to give each server its own GPU.

The device actually used is reported as the backend's device name, for example
`METAL: Apple M4 Pro (GPU - 20 cores)`.

### Sampling

Path tracing is nearly all of a BlenderProc frame's cost, about 1 ms per sample
for a 256×256 frame on an M4 Pro, so these settings decide its speed:

| Setting | Default | |
|---|---|---|
| `samples` | 128 | Maximum Cycles samples per pixel. |
| `noise_threshold` | 0.01 | Adaptive sampling stops a pixel once its noise is below this; 0 always takes every sample. |
| `denoiser` | `oidn` | `oidn` (Intel Open Image Denoise), `optix` (needs an OptiX device) or `none`. |
| `glossy_bounces` | 1 | Light bounces off glossy surfaces. BlenderProc's own default, 0, leaves metals reflecting only direct light: a mirror shows nothing of its surroundings. |

The denoiser runs inside Cycles, on the GPU where Blender supports it, and
only on RGB: depth and instance labels are never denoised or sampled
differently. BlenderProc's own defaults (1024 samples, OIDN as a CPU
compositor node) are not used. On five `small`-preset scenes the defaults
render 640×480 frames at 51.5 dB PSNR against 4096 undenoised samples,
with 99% of pixels within 2/255 and identical instance labels, about four
times faster than BlenderProc's defaults. The backend's provenance records
the three values.

### Lighting conventions

The worker matches wgpu's units: a sun's strength is `illuminance_lux / 1000 × π`,
because Blender's diffuse BSDF divides sun irradiance by π, so 1000 lux lights
a white diffuse surface facing the sun to 1.0 in both backends. The view
transform is Standard rather than Blender's default AgX. Ambient light is a
uniform world colour, or the environment map as the world texture. Cycles
occludes it exactly and adds light bounced between surfaces; wgpu
approximates both in screen space. Base-color textures multiply the material's factor in both backends.
Point-light power has not been calibrated between the backends. Where a ray
hits nothing, Blender reports a depth of 1e10; the worker writes 0 for any depth
beyond the camera's far plane, the same background value wgpu writes.

The optional runtime environment is pinned in
`tools/blenderproc-worker/pyproject.toml` and `uv.lock`; it is not linked into
or vendored with the Rust binaries. The lock resolves BlenderProc 2.8.0 and
records its complete Python dependency graph. A live worker reports both the
installed BlenderProc package version and Blender version in backend
provenance.

The real analytic-triangle cross-backend gate renders the same 64x64 request
through Metal wgpu and BlenderProc, validates both canonical staging trees, and
compares geometry rather than RGB. On the development M4 Pro it passed with a
one-pixel-tolerant visible-mask IoU of 1.0, zero median depth difference, and a
0.000119 mm p99 depth difference. The gate requires IoU at least 0.97, median
depth difference at most 1 mm, and p99 at most 5 mm. A second request through
the same Blender process also passed, covering persistent compositor-output
reuse and fresh per-request instance labels.

Capability detection never falls back to wgpu. Run
`cargo run -p bop-cli --features blenderproc -- doctor --renderer blenderproc`;
an absent executable, failed handshake, or incompatible runtime exits with
capability code 3. The ignored `real_worker_renders_and_validates_the_canonical_three_channels`
test becomes the real-runtime acceptance gate after installation. Fake workers
cover bounded handshake timeout, malformed JSON, persistent reuse, exact
request IDs, one crash/reprepare retry, canonical encoding, and shutdown in
normal CI.

## Multiple GPUs

Each renderer worker (`[run] renderer_workers`) gets one GPU, taken in turn:
worker `i` uses GPU `i` modulo the number of GPUs. With two GPUs and four
workers, workers 0 and 2 share the first and workers 1 and 3 the second. A
single-GPU machine is unaffected.

- **wgpu** spreads over the adapters that tie for the best match: the same
  device class and power preference on the same backend, for example two
  identical discrete GPUs on Vulkan. An integrated GPU next to a discrete one
  is not used, and neither is the OpenGL view of a GPU that Vulkan also
  exposes.
- **BlenderProc** spreads over the GPUs that `device_selection` selects, so
  `CUDA:0,1` limits the workers to those two. With one renderer worker, that
  worker renders on all selected GPUs at once, as Cycles does by default.

Rendering is CPU-bound for wgpu (encoding and validation dominate), so extra
GPUs speed wgpu up only when the CPU has cores to spare; BlenderProc, which is
path-tracing-bound, scales close to the number of GPUs. Datasets rendered on
different GPU models are not pixel-identical; see above.

## Verification

Normal repository gates are:

```text
cargo test --workspace --locked
cargo test --workspace --no-default-features --features reproducible --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo run -p xtask -- schema --check
git diff --check
```

Metal tests must run outside restrictive filesystem sandboxes because macOS may
otherwise enumerate no adapters. BlenderProc absence is reported as backend
unavailability; it must never be recorded as a passing real-renderer test.

# Renderer architecture and operation

Both renderers implement `bpp_core::Renderer` with the same
`ResolvedAssetCatalog`, `RenderSceneRequest`, and `SceneStagingArea` boundary.
There is no implicit fallback: selecting BlenderProc and failing to start it is
an error, not permission to produce a wgpu dataset.

## Coordinate and output contract

Scene geometry is metres in the canonical right-handed Z-up world. Camera
snapshots store OpenCV camera-from-world extrinsics: +X right, +Y down, +Z
forward. RGB is 8-bit sRGB PNG, depth is finite nonnegative camera-Z metres in
a single-channel `Z` float EXR, and visible instance labels are lossless
16-bit grayscale PNG with zero reserved for background.

Before publication, `bpp-dataset` decodes every requested frame and checks its
dimensions and representation. It rejects negative/non-finite depth and labels
not declared by the scene snapshot. It then writes `checksums.json`, containing
sorted BLAKE3 checksums for scene metadata, camera metadata, and every channel.
Only after those checks does the staging directory become a committed scene.

## wgpu backend

`WgpuRenderer` owns one persistent headless instance, adapter, device, queue,
pipeline, and uploaded asset catalog. Adapter selection is deterministic within
the requested CPU/GPU and power class. Explicit classes are hard requirements.
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

The shader supports base-color factors, metallic/roughness response, ambient
light, and up to eight directional plus eight inverse-square point lights. The
first directional light owns one configurable orthographic shadow map whose
volume is derived from transformed scene bounds; additional lights remain
unshadowed. Base-color textures are rejected during preparation instead of
being silently ignored. Cross-GPU pixel identity is not claimed. RGB is
intended for fast deterministic rasterized training imagery; depth, labels,
geometry, and metadata are the strict contracts.

Hardware acceptance on Apple Metal:

```text
cargo test -p bpp-render-wgpu --release --locked --lib \
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
omit that field and advertise `timestamp_queries=false` in backend provenance.

## BlenderProc backend

`BlenderProcRenderer` starts one persistent external process using:

```text
<executable_path> run <worker_path> -- --bpp-worker
```

The worker is [workers/blenderproc_worker.py](../../workers/blenderproc_worker.py).
Worker replies use `BPP1<TAB>` followed by strict protocol-v1 JSON. The Rust
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

The optional runtime environment is pinned in
`tools/blenderproc-worker/pyproject.toml` and `uv.lock`; it is not linked into
or vendored with the Rust binaries. The lock resolves BlenderProc 2.7.0 and
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
`cargo run -p bpp-cli --features blenderproc -- doctor --renderer blenderproc`;
an absent executable, failed handshake, or incompatible runtime exits with
capability code 3. The ignored `real_worker_renders_and_validates_the_canonical_three_channels`
test becomes the real-runtime acceptance gate after installation. Fake workers
cover bounded handshake timeout, malformed JSON, persistent reuse, exact
request IDs, one crash/reprepare retry, canonical encoding, and shutdown in
normal CI.

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

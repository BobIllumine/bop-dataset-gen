# Benchmark results: wgpu and BlenderProc

Measured on 2026-10-20 on an Apple M4 Pro (14 CPU cores, 20-core GPU, Metal)
with `bop-gen bench` and the `smoke` and `small` presets. Each backend renders
the same snapshot corpus with one renderer instance; the method is described
in [Benchmarks](benchmarks.md). The full reports are `report.md` and
`report.json` in each benchmark's output directory.

Both backends ran with their current defaults. wgpu renders PBR materials with
2048-pixel shadow maps, ambient occlusion, screen-space reflections, and FXAA.
BlenderProc 2.8.0 on Blender 4.2.1 LTS uses the
[sampling defaults](renderers.md#sampling): 128 samples, noise threshold 0.01,
and OIDN. Objects without a file material get sampled
[material defaults](configuration.md#materials) in both backends.

## Throughput

| Preset | Frames | Backend | Scenes/s | p50 scene | p95 scene | Render/scene | Encode/scene |
|---|---|---|---:|---:|---:|---:|---:|
| `smoke` | 8 × 256×256, 5 repetitions | wgpu | 62.1 | 10 ms | 14 ms | 2.6 ms | 1.2 ms |
| | | BlenderProc | 2.79 | 345 ms | 386 ms | 342 ms | 1.3 ms |
| `small` | 100 × 640×480, 3 repetitions | wgpu | 49.4 | 20 ms | 23 ms | 7.0 ms | 4.1 ms |
| | | BlenderProc | 1.02 | 977 ms | 1044 ms | 952 ms | 5.4 ms |

Both presets use one camera, so scenes and frames per second are equal. Render
and encode are the renderer's median per scene; the rest of a scene's latency
is validation and the atomic commit.

- **wgpu** is about 48 times faster than BlenderProc on `small`. The lighting
  features cost it about a fifth of its earlier throughput (60.3 scenes/s with
  only shadows and FXAA).
- **BlenderProc** is path-tracing-bound: rendering is 97% of its scene time.
  Moving to Blender 4.2 and the new sampling defaults made it 2.4 times faster
  on `small` (0.41 scenes/s before) and 2.7 times faster on `smoke` (1.10).
- Physics is the same for both backends and is not a bottleneck: 100 `small`
  scenes simulate in 0.27 s on 8 workers.
- A `small` dataset is 124 MiB from wgpu and 140 MiB from BlenderProc, with
  the default `fast` PNG compression.

## Where BlenderProc's time goes

Timed inside Blender, path tracing is nearly all of a frame: about 1 ms per
sample for a 256×256 frame plus about 100 ms of fixed cost per frame, while
reading the rendered files back takes about 8 ms. BlenderProc's defaults take
up to 1024 samples per pixel and denoise afterwards; the defaults now take at
most 128 and denoise inside Cycles.

Rendering all of a scene's cameras in one Cycles call instead of one call per
camera made no difference (3.51 s and 3.52 s per four-camera scene): Blender
already keeps the scene between calls.

Blender 4.2 path-traces about 24% faster than 3.5 (25.0 s and 32.9 s for a
4096-sample 640×480 frame), but its fixed cost per frame is higher, so at 128
samples the two versions take about the same time (1.10 s and 1.07 s).

## Image quality

Five `small`-preset scenes were rendered with the new defaults and with 4096
samples, a 0.001 noise threshold and no denoiser as the reference:

- RGB: 51.5 to 51.7 dB PSNR per frame; 99% of pixels within 2/255.
- Instance labels: identical.

On a synthetic scene (ten cubes, metallic 0.4, roughness 0.5), BlenderProc's
old defaults scored 49.0 dB against the same kind of reference at 640×480 and
128 samples with OIDN 48.0 dB, at a fifth of the time. Depth is not affected
by sampling: the cross-backend geometry gate still measures a visible-mask IoU
of 1 and a p99 depth difference of 0.1 µm against wgpu.

## Caveats

- One machine, one run each. Treat differences of a few percent as noise.
- Only single-camera presets were measured. `medium` (500 scenes × 4 cameras)
  would take about 35 minutes per BlenderProc repetition even at the new
  defaults.
- The BlenderProc worker's peak RSS is reported as 31 MiB. That is the
  `blenderproc` launcher process, not Blender itself, so the figure
  understates BlenderProc's memory use.
- `bench` uses one renderer instance per backend. `generate` with
  `renderer_workers > 1` runs several renderers, spread over the GPUs when
  there are several (see [Multiple GPUs](renderers.md#multiple-gpus)); how
  much that helps has not been measured.

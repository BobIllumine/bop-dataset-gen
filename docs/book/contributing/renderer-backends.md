# Renderer backends

`crates/bop-render-api/`, `crates/bop-render-wgpu/`,
`crates/bop-render-blenderproc/`, `tools/blenderproc-worker/` ·
code reference: [api](../api/bop_render_api/index.html),
[wgpu](../api/bop_render_wgpu/index.html),
[BlenderProc](../api/bop_render_blenderproc/index.html)

[Renderers](../renderers.md) explains what each backend draws and how to
configure it. This chapter is about the code.

## The contract

`bop_render_api::Renderer` has five methods:

| Method | Called | Must |
|---|---|---|
| `info` | any time | Return the same `BackendInfo` for the renderer's whole life. It goes into the manifest, so resume compares it. |
| `capabilities` | before each scene | Report the channels and camera count this instance supports. |
| `prepare` | once per run | Upload or transfer the whole asset catalog. |
| `render_scene` | once per scene | Write every requested frame into the staging area it is given, and nothing anywhere else. |
| `flush` | at the end and at durability boundaries | Finish all queued work. |

A renderer:

- receives an immutable `RenderSceneRequest` and never sees physics or the
  configuration;
- writes frames only through the `bop-dataset` encoders, so both backends
  produce the same formats;
- returns a typed `RenderError` and never falls back to another backend or
  device on its own;
- is owned by exactly one worker thread (`Send`, not `Sync`).

The pipeline checks the rest: the report, every decoded frame, and the
required files before commit.

## wgpu

`lib.rs` holds `WgpuRenderer`: device setup, adapter selection
(`worker_adapter`, `adapter_rank`), catalog preparation, and the per-camera
pass sequence. The other modules hold one concern each:

| Module | Role |
|---|---|
| `pipelines.rs` | Render pipelines, loading the WGSL shaders that sit next to it in `src/` (`scene.wgsl`, `shadow.wgsl`, `ao.wgsl`, `reflections.wgsl`, `fxaa.wgsl`, …). |
| `geometry.rs` | Vertex and index buffers for prepared assets. |
| `textures.rs` | Texture upload and samplers. |
| `environment.rs` | Environment map prefiltering. |
| `readback.rs` | The three-frame readback ring. |
| `gpu_types.rs` | `bytemuck` structs shared with the shaders. |
| `errors.rs` | Initialization and scene errors. |

Tests that need a GPU are `#[ignore]`d and run in the hardware workflow; see
[Development and CI](../development.md).

## BlenderProc

`lib.rs` starts and supervises one persistent worker process per renderer:

- `Worker` spawns `blenderproc run <worker> -- --bop-worker` and owns its
  pipes. Replies are lines prefixed `BOP1<TAB>`; anything else is kept as
  diagnostics.
- `WorkerMessage` and `WorkerReply` are the protocol. Every request has an
  ID, and a reply must match it.
- `render_scene` sends the request; if the worker died, it restarts it once,
  prepares the catalog again, and resends. Protocol and content errors are
  never retried.
- The worker writes raw buffers; `encode_raw_outputs` checks their sizes and
  encodes them with the shared encoders.

The Python side is `tools/blenderproc-worker/blenderproc_worker.py`. A
protocol change has to change both sides and the protocol version together.
`tests/worker_supervision.rs` and `tests/protocol.rs` use fake workers, so
they run without Blender.

## Adding a backend

1. Create `crates/bop-render-<name>/`, depending on `bop-core`, `bop-assets`,
   `bop-dataset`, and `bop-render-api` only.
2. Implement `Renderer`, writing frames with `RgbFrame`, `DepthFrame`, and
   `InstanceFrame`.
3. Add a feature and a `RendererKind` variant in `bop-run`, and the renderer
   name in the CLI and in `submission::parse_renderer` in the server.
4. Render the same snapshot corpus with the new backend and an existing one,
   and compare the geometry: `bop-render-blenderproc/tests/cross_backend_geometry.rs`
   is the pattern.

## Caveats

- `bop-render-wgpu/src/lib.rs` is about 1,800 lines of code before its
  tests, far over the size the [architecture rules](rules.md) allow. It is
  due to be split.
- Both backends build bin geometry themselves: wgpu in `push_bin_draws`,
  from `analytic_bin_snapshot` in `bop-assets`, and the BlenderProc worker
  in Python. That moves to the general scene model.
- The render device is part of `BackendInfo`, so a run must be resumed on
  the same kind of device. This also limits which servers can continue a job.

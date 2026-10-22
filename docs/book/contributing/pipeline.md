# bop-pipeline

`crates/bop-pipeline/` · [code reference](../api/bop_pipeline/index.html)

## What it owns

Putting the stages together: physics produces snapshots, a render request
planner adds cameras and materials, renderers render, and the dataset layer
commits. It owns the serial and parallel versions of each combination,
cancellation, and the benchmark harness behind `bop-gen bench`.

It does not own any stage itself. Physics is `bop-physics-rapier`, rendering
is whatever implements `Renderer`, and every file format belongs to
`bop-dataset`.

## Main types

| Type | Role |
|---|---|
| `SerialDatasetRenderer` (`lib.rs`) | The transaction around one scene: staging, metadata, rendering, checks, commit. Every pipeline renders through it. |
| `SerialGenerationPipeline` (`serial_generation.rs`) | Physics and rendering on the calling thread, one scene at a time. The reference implementation. |
| `ParallelGenerationPipeline` (`parallel_generation.rs`) | Physics workers and renderer workers joined by a bounded channel. Must produce the same bytes as the serial pipeline. |
| `SerialSnapshotSimulator`, `ParallelSnapshotSimulator` | Physics only: write a snapshot corpus (`bop-gen simulate`). |
| `SerialSnapshotRenderer`, `ParallelSnapshotRenderer` | Rendering only: render a stored corpus (`bop-gen render`). |
| `ParallelPhysicsExecutor` (`parallel_physics.rs`) | The physics worker pool: atomic scene claiming, bounded channel, first-error-wins. |
| `render_pool` (`render_pool.rs`) | One thread per renderer, all reading the same channel. |
| `RenderRequestPlanner` (`request_planner.rs`) | Snapshot to `RenderSceneRequest`: cameras, materials, lights. |
| `prepare_asset_catalog` (`asset_preparation.rs`) | Loads and prepares every configured asset once per run. |
| `CancellationToken` (`cancellation.rs`) | A shared flag checked before each new scene. |

## Rules this crate keeps

- **Serial is the reference.** Each parallel type has a serial twin, and the
  tests in `tests/fake_renderer_pipeline.rs` compare their output byte for
  byte. A change that only touches the parallel path must still pass that
  comparison.
- **Commits are per scene.** `SerialDatasetRenderer::render_and_commit`
  makes each scene durable before the next one starts. This is deliberate:
  durability over throughput. Group commit is used only for snapshot
  corpora, where the snapshots can be regenerated cheaply.
- **Bounded memory.** At most `queue_capacity + simulation_workers` snapshots
  exist at once; see [Concurrency](../concurrency.md).
- **Cancellation is cooperative.** It is checked before claiming or starting
  a scene. A scene already in progress always reaches its commit or its
  cleanup, never a half-written state.
- **One backend per pool.** `dataset_renderers` rejects a pool whose
  renderers report different `BackendInfo`, because the manifest records a
  single backend.

## Changing it

- New stage or new pipeline shape: add the serial version first, then the
  parallel one, and extend the byte-for-byte comparison.
- New per-scene output file: write it inside `render_and_commit` before
  `validate_and_write_checksums`, and add it to the required artifacts in
  `bop-dataset` so commit and validation check it.
- New seeded decision: derive it from its own `SeedStage` (see
  [Determinism](../determinism.md)), never from another stage's stream.

## Caveats

- The commit observer runs on the renderer thread that committed the scene,
  and that renderer waits for it. Keep observers cheap: the server's only
  sends a path over a channel.
- `RenderRequestPlanner` reads bin geometry for camera targets. It is part
  of the scene model redesign.
- The benchmark harness (`bench_runner.rs`, `bench_report.rs`,
  `benchmark.rs`) lives here because it drives the same stages, but it is
  not used by generation. See [Benchmarks](../benchmarks.md).

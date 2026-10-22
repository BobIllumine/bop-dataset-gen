# Architecture

## Data flow

One scene moves through the system like this:

```text
run.toml ──► RunConfig ──► RunPlan                         (bop-core)
                              │
asset files ──► prepared catalog (meshes, materials,       (bop-assets)
                convex hulls; cached on disk)
                              │
SceneId ──► seeds ──► population ──► placement ──► Rapier settle
                              │                            (bop-physics-rapier)
                              ▼
                       SceneSnapshot   ◄── immutable, renderer-neutral
                              │
             RenderRequestPlanner: + cameras, lights       (bop-pipeline)
                              │
              ┌───────────────┴───────────────┐
         wgpu renderer                 BlenderProc renderer
         (bop-render-wgpu)             (bop-render-blenderproc)
              └───────────────┬───────────────┘
                              ▼
        .staging/<scene> ──► validate ──► atomic rename     (bop-dataset)
                              │
                        scenes/<scene id>/
```

Physics and rendering never talk to each other directly. Physics produces a
`SceneSnapshot`; renderers only read snapshots. That boundary is what lets the
CLI split a run in two: `bop-gen simulate` writes snapshots to a *corpus* on disk,
and `bop-gen render` renders a corpus later, with either backend, without running
physics again.

## Crates

| Crate | Owns |
|---|---|
| `bop-core` | Identifiers, seed derivation, transforms, cameras, scene snapshots, render requests and reports, `RenderError`, and the validated run configuration. |
| `bop-assets` | STL/GLB loading, unit and axis normalization, materials, convex decomposition, and the content-addressed cache. |
| `bop-physics-rapier` | Population and placement planning, the Rapier world, settling, and rejection. |
| `bop-render-api` | The `Renderer` trait every backend implements. |
| `bop-render-wgpu` | The in-process GPU renderer. |
| `bop-render-blenderproc` | Supervision of the persistent BlenderProc worker process. |
| `bop-dataset` | On-disk formats: staging, atomic commit, group commit, resume, validation, and snapshot corpora. |
| `bop-pipeline` | Composition of the stages, serially and in parallel. |
| `bop-run` | Choosing a renderer backend and a pipeline for a loaded run; shared by the CLI and the server. |
| `bop-cli` | The `bop-gen` binary: argument parsing, exit codes, logging. |
| `bop-server` | The HTTP job server: jobs, assets, storage, authentication. |
| `xtask` | Repository maintenance, such as writing the configuration schema. |

Dependencies point downward. `bop-core` depends on no other workspace crate.
`bop-pipeline` is generic over any `Renderer` and does not depend on either
backend; `bop-run` is where the concrete renderers are named. The
[contributor guide](contributing/crates.md) has a chapter per crate.

## Serial and parallel pipelines

Each stage combination exists twice. The serial types
(`SerialGenerationPipeline`, `SerialSnapshotSimulator`,
`SerialSnapshotRenderer`) are the simple reference implementations. The
parallel types (`ParallelGenerationPipeline`, `ParallelSnapshotSimulator`,
`ParallelSnapshotRenderer`) must produce byte-identical output, and the tests
compare the two directly. The CLI uses a serial type when the configuration
asks for one simulation worker and one renderer worker, and a parallel type
otherwise.

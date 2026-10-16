# Architecture

## Data flow

One scene moves through the system like this:

```text
run.toml ──► RunConfig ──► RunPlan                         (bpp-core)
                              │
asset files ──► prepared catalog (meshes, materials,       (bpp-assets)
                convex hulls; cached on disk)
                              │
SceneId ──► seeds ──► population ──► placement ──► Rapier settle
                              │                            (bpp-physics-rapier)
                              ▼
                       SceneSnapshot   ◄── immutable, renderer-neutral
                              │
             RenderRequestPlanner: + cameras, lights       (bpp-pipeline)
                              │
              ┌───────────────┴───────────────┐
         wgpu renderer                 BlenderProc renderer
         (bpp-render-wgpu)             (bpp-render-blenderproc)
              └───────────────┬───────────────┘
                              ▼
        .staging/<scene> ──► validate ──► atomic rename     (bpp-dataset)
                              │
                        scenes/<scene id>/
```

Physics and rendering never talk to each other directly. Physics produces a
`SceneSnapshot`; renderers only read snapshots. That boundary is what lets the
CLI split a run in two: `bpp simulate` writes snapshots to a *corpus* on disk,
and `bpp render` renders a corpus later, with either backend, without running
physics again.

## Crates

| Crate | Owns |
|---|---|
| `bpp-core` | Identifiers, seed derivation, transforms, cameras, scene snapshots, render requests and reports, `RenderError`, and the validated run configuration. |
| `bpp-assets` | STL/GLB loading, unit and axis normalization, materials, convex decomposition, and the content-addressed cache. |
| `bpp-physics-rapier` | Population and placement planning, the Rapier world, settling, and rejection. |
| `bpp-render-api` | The `Renderer` trait every backend implements. |
| `bpp-render-wgpu` | The in-process GPU renderer. |
| `bpp-render-blenderproc` | Supervision of the persistent BlenderProc worker process. |
| `bpp-dataset` | On-disk formats: staging, atomic commit, group commit, resume, validation, and snapshot corpora. |
| `bpp-pipeline` | Composition of the stages, serially and in parallel. |
| `bpp-cli` | The `bpp` binary: argument parsing, exit codes, logging, and choosing a renderer. |

Dependencies point downward. `bpp-core` depends on no other workspace crate.
`bpp-pipeline` is generic over any `Renderer` and does not depend on either
backend; only `bpp-cli` names the concrete renderers.

## Serial and parallel pipelines

Each stage combination exists twice. The serial types
(`SerialGenerationPipeline`, `SerialSnapshotSimulator`,
`SerialSnapshotRenderer`) are the simple reference implementations. The
parallel types (`ParallelGenerationPipeline`, `ParallelSnapshotSimulator`,
`ParallelSnapshotRenderer`) must produce byte-identical output, and the tests
compare the two directly. The CLI uses a serial type when the configuration
asks for one simulation worker and one renderer worker, and a parallel type
otherwise.

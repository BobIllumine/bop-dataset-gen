# Crates

Each crate has one job and a short list of things it must not know about.
The chapters in this section cover what each crate owns, its main types, how
to change it safely, and its caveats. The code reference has the details of
every item.

| Crate | Job | Chapter |
|---|---|---|
| `bop-core` | Shared vocabulary: identifiers, seeds, transforms, cameras, scene snapshots, render requests and reports, the validated run configuration. Depends on no other workspace crate. | Pending the scene model redesign |
| `bop-assets` | Load and normalize STL and GLB files, extract materials and textures, build and cache convex decompositions. | Pending the scene model redesign |
| `bop-physics-rapier` | Choose which objects appear and where, drop and settle them in Rapier, and reject scenes that do not settle. | Pending the scene model redesign |
| `bop-render-api` | The `Renderer` trait. | [Renderer backends](renderer-backends.md) |
| `bop-render-wgpu` | The in-process GPU rasterizer. | [Renderer backends](renderer-backends.md) |
| `bop-render-blenderproc` | Supervision of the external BlenderProc worker. | [Renderer backends](renderer-backends.md) |
| `bop-dataset` | Everything on disk: staging, atomic commit, resume, validation, snapshot corpora, BOP export. | [bop-dataset](dataset.md) |
| `bop-pipeline` | Composing the stages, serially and in parallel, with cancellation. Also the benchmark harness. | [bop-pipeline](pipeline.md) |
| `bop-run` | Choosing a renderer backend and a pipeline for a loaded run. | [bop-run and bop-cli](run-and-cli.md) |
| `bop-cli` | The `bop-gen` binary. | [bop-run and bop-cli](run-and-cli.md) |
| `bop-server` | The HTTP job server. | [Server internals](server.md) |
| `xtask` | Repository maintenance: `cargo run -p xtask -- schema` writes the configuration JSON Schema. | — |

`tools/blenderproc-worker/` is the Python program that runs inside
BlenderProc; it is covered with the BlenderProc backend.

## Allowed dependencies

```text
bop-core
  ├── bop-assets
  │     └── bop-physics-rapier
  ├── bop-dataset
  └── bop-render-api            (also uses bop-assets, bop-dataset)
        ├── bop-render-wgpu
        └── bop-render-blenderproc
bop-pipeline                    (core, assets, dataset, physics, render-api)
bop-run                         (pipeline, render-api, both backends behind features)
bop-cli, bop-server             (bop-run and the layers below it)
```

A crate only depends on crates above it in this list. In particular:

- `bop-core` depends on no workspace crate.
- Physics and the renderers never depend on each other.
- `bop-pipeline` is generic over `Renderer` and never names a backend.
- Only `bop-run` names the concrete backends, so the CLI and the server
  cannot drift apart in how they run a job.

## Why core, assets, and physics wait

These three crates model the scene as objects dropped into bins: `BinPlan`
and `BinConfig` in the configuration, `BinSnapshot` in every scene, cameras
aimed at bin-local targets. Making the generator general means replacing
that model, so their chapters will be written against the new design instead
of being written twice. Until then, read their crate overviews in the code
reference and the [Configuration](../configuration.md) and
[Determinism](../determinism.md) chapters.

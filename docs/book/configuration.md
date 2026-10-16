# Configuration

A run is described by one strict TOML file. Unknown fields are rejected, and
every value is validated before anything runs. `bpp schema` prints the full
JSON Schema; `schemas/run-config.schema.json` is the committed copy, and CI
fails if the two drift apart.

## Sections

| Section | Controls |
|---|---|
| `[run]` | Master seed, number of valid scenes, attempt limits, worker counts, queue capacity, and the asset cache path. |
| `[bin]` | Inner dimensions, wall and floor thickness, pose, and contact properties of the bin. |
| `[[assets]]` | Each object model: source file, unit scale, model transform, mixture weight, physical-property distributions, and convex-decomposition parameters. |
| `[population]` | How many objects a scene gets, which assets they are drawn from, and the spawn volume. |
| `[physics]` | Rapier timestep, solver settings, and the settling and rejection thresholds. |
| `[[cameras]]` | Image size, clip planes, projection, and a fixed or sampled pose. |
| `[appearance]` | Background, ambient, directional and point lights, and default materials. |
| `[renderers.wgpu]`, `[renderers.blenderproc]` | Backend-specific settings. |
| `[output]` | Which channels to write: RGB, depth, visible instances. |

Distributions are written as tables such as
`{ kind = "fixed", value = 0.5 }`; the schema lists every kind.

## Parallelism settings

```toml
[run]
simulation_workers = 8   # Rapier worlds simulating scenes in parallel
renderer_workers = 4     # renderer instances, each on its own thread
queue_capacity = 16      # simulated scenes allowed to wait for a renderer
```

Worker counts never change the output; see [Concurrency](concurrency.md).
Each renderer worker is a full renderer instance: one wgpu device, or one
BlenderProc process.

## Command-line overrides

`simulate`, `render`, and `generate` accept `--seed N` and `--scenes N`. An
override is written into the *resolved configuration*, the exact bytes stored
beside the output and used for provenance and resume compatibility. Without
overrides, the resolved configuration is the file's bytes unchanged.

Because a corpus and the datasets rendered from it must share one resolved
configuration, pass the same overrides to `bpp render` that you passed to
`bpp simulate`. Output paths are deliberately not part of the resolved
configuration, so a corpus can be rendered into any directory.

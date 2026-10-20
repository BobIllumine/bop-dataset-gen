# Command line

```text
bop-gen [--log-format human|json] SUBCOMMAND

bop-gen doctor [--renderer wgpu|blenderproc|all]
bop-gen schema
bop-gen prepare-assets --config FILE
bop-gen simulate --config FILE --output CORPUS [--seed N] [--scenes N]
bop-gen render --config FILE --snapshots CORPUS --renderer BACKEND --output DATASET [--seed N] [--scenes N]
bop-gen generate --config FILE --renderer BACKEND --output DATASET [--resume] [--seed N] [--scenes N]
bop-gen validate DATASET
bop-gen export bop DATASET --output DIRECTORY [--depth-scale MM]
bop-gen bench --config FILE --renderers BACKEND[,BACKEND] --output DIRECTORY [--repetitions N]
bop-gen optimize-asset SOURCE --output FILE.glb [--max-deviation-mm MM] [--crease-angle DEG] [--unit-scale M]
```

Each command prints one JSON result object on stdout. Logs go to stderr.

To try it, generate the smoke preset and export it:

```text
bop-gen generate --config examples/presets/smoke.toml --renderer wgpu --output out/smoke
bop-gen validate out/smoke
bop-gen export bop out/smoke --output out/smoke-bop
```

See [BOP export](bop.md) and [Benchmarks](benchmarks.md) for `export bop` and
`bench`.

## Optimizing a mesh

`optimize-asset` makes a smaller GLB from an STL or GLB, for use as an asset:

1. **Weld.** Corners at bit-identical positions become one vertex. Meshes
   converted from STL store each triangle's corners separately, so this alone
   makes them about three times smaller without moving anything.
2. **Simplify.** Triangles are collapsed while the surface stays within
   `--max-deviation-mm` (default 0.1) of the source. The deviation is
   measured, not estimated: the largest distance from a source vertex or
   triangle centre to the result, and from each result triangle's centre and
   edge midpoints to the source. If a pass exceeds the budget, it is retried
   with half the target. `0` only welds.
3. **Normals.** Normals are rebuilt from the faces around each vertex. Faces
   meeting at more than `--crease-angle` degrees (default 30) keep separate
   normals, so edges stay sharp.

The result is in metres, with the source's materials. STL has no units, so
`--unit-scale` (metres per unit, 0.001 for millimetres) is required for STL
and ignored for GLB. Textured assets are refused: their UV seams would need
preserving. The output must not exist yet.

At a 640×480 frame of a 10 cm object, one pixel covers about 1 mm, so
sub-millimetre budgets do not change images visibly. For the example cup and
nippel (84k and 117k triangles):

| Budget | Cup | Nippel |
|---|---|---|
| 0 (weld only) | 2.1 MB, 84k triangles | 2.8 MB, 117k triangles |
| 0.1 mm | 2.0 MB, 71k | 2.7 MB, 94k |
| 0.5 mm | 0.5 MB, 11k | 1.0 MB, 24k |

The sources are 6.7 and 9.2 MB.

## Interrupting a run

The first Ctrl-C (or SIGTERM) during `simulate`, `render`, or `generate`
stops new scenes from starting. Scenes already in flight finish and
commit, the JSON result is printed with `"cancelled": true`, and the command
exits with code 130. Run the same command again to continue: `simulate` and
`render` resume on their own, `generate` needs `--resume`. A second Ctrl-C
exits immediately; that is also safe, because scenes are published only by
atomic rename.

BlenderProc workers run in their own process group, so the terminal's Ctrl-C
reaches only `bop-gen`, which then finishes the scene the worker is rendering.
`bench` has no interrupt handling: Ctrl-C stops it at once, and its scratch
output under `DIRECTORY` is simply incomplete.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 2 | Command-line or configuration error |
| 3 | The requested capability or runtime is unavailable |
| 4 | The requested number of valid scenes could not be produced |
| 5 | Dataset validation or export failure |
| 6 | Renderer or worker failure |
| 130 | Interrupted by Ctrl-C or SIGTERM; committed scenes are kept |

## Logging

Logs are human-readable by default. `--log-format json` writes one JSON object
per line instead. The `BOP_LOG` environment variable sets the filter using
`tracing` syntax and defaults to `info`. Use `BOP_LOG=debug` to see each
simulated and committed scene.

## Environment variables

| Variable | Effect |
|---|---|
| `BOP_LOG` | Log filter, for example `debug` or `bop_pipeline=debug`. |
| `BOP_BLENDERPROC_EXECUTABLE` | BlenderProc executable to run instead of the configured one. |
| `BOP_BLENDERPROC_WORKER` | Worker script for `doctor`. Otherwise `doctor` looks in `blenderproc-worker/` next to the binary, then in the source tree. |

# Benchmarks

```text
bop-gen bench --config FILE --renderers BACKEND[,BACKEND] --output DIRECTORY [--repetitions N]
```

## Presets

`examples/presets/` holds four benchmark configurations. The smoke preset is
also the quickstart configuration.

| Preset | Scenes | Objects | Cameras | Resolution |
|---|---:|---:|---:|---:|
| `smoke` | 8 | 10 | 1 | 256×256 |
| `small` | 100 | 30 | 1 | 640×480 |
| `medium` | 500 | 65 mixed | 4 | 640×480 |
| `stress` | 200 | 100 mixed | 8 | 1024×1024 |

"Mixed" draws equally from a cube, a hexagonal prism, and a concave L-bracket
(`examples/assets/`). Single-camera presets look straight down; the others
sample cameras on a shell around the bin.

## Method

1. **Preparation.** Assets are prepared twice. The first call is cold only if
   the convex-decomposition cache did not exist yet (the report says which);
   the second always reuses the cache.
2. **Physics.** One snapshot corpus is simulated into `DIRECTORY/corpus` with
   the configured worker count and timed. A later run with the same resolved
   configuration reuses the corpus, so every run renders identical scenes; its
   physics is then reported untimed.
3. **Warm-up.** Each backend is prepared once and renders two scenes that are
   not counted.
4. **Measurement.** Each backend renders the whole corpus into a fresh dataset,
   `--repetitions` times (default 5), with one renderer instance. Within a
   repetition the backends run in a seeded order that rotates every
   repetition, so no backend always goes first. Only the latest repetition's
   dataset is kept, under `DIRECTORY/runs/<backend>`.

## Report

`report.json` (schema version 1) and the generated `report.md` contain:

- scenes and frames per second (median over repetitions);
- p50 and p95 per-scene latency over every measured scene, wall-clock from
  render start to atomic commit;
- renderer-reported render, readback, encode and (where available) GPU seconds
  per scene;
- physics wall time, scenes per second, mean steps per scene, and rejection
  rate;
- output bytes of one dataset;
- peak RSS of `bop-gen`, and of the largest renderer worker process once it has
  exited;
- provenance: git revision and dirty state, build features and profile, OS,
  CPU, adapter details, and renderer versions.

## Reading the numbers

- Rendering is measured with one renderer instance, to compare backends on
  equal terms. `generate` with `renderer_workers > 1` runs several and is
  faster.
- Physics and rendering are measured separately. A full `generate` overlaps
  them, so its wall time is not their sum.
- Peak RSS is a high-water mark for the whole process, not per backend.
- Treat the numbers as measurements. There are no pass/fail thresholds until
  real baselines exist, and backends should only be compared within one report.

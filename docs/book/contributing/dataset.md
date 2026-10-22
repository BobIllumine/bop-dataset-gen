# bop-dataset

`crates/bop-dataset/` · [code reference](../api/bop_dataset/index.html)

## What it owns

Every byte the generator writes to disk: the dataset layout, the frame
encoders, staging and atomic commit, durability, resume, validation, snapshot
corpora, and the BOP export. If a format changes, it changes here.

It does not know about physics, renderers, or the server. Renderers write
frames through its encoders and its staging paths; the server uploads what
it committed.

## Modules

| Module | Role |
|---|---|
| `frames.rs` | `RgbFrame`, `DepthFrame`, `InstanceFrame`: the only PNG and EXR encoders and decoders. Both backends use them. |
| `staging.rs` | `SceneStagingArea`: a private directory under `.staging/`, its canonical paths, and `commit`, which checks required files, makes them durable, and renames the directory into `scenes/`. |
| `durability.rs` | `DurableBatch`: many files made durable with one device flush. |
| `validation.rs` | `validate_and_write_checksums` before commit; `validate_dataset` for a whole dataset. |
| `resume.rs` | `DatasetManifest` (`run.json`), `DatasetResumeIndex`, `RunEnvironment` (`environment.json`), and atomic installation of small files. |
| `models.rs` | Canonical meshes stored under `assets/` so exporters never need source files. |
| `snapshot_corpus.rs` | Reading and writing physics-only corpora. |
| `bop.rs` | `export_bop` and `validate_bop`. |

## Rules this crate keeps

- **A scene is all or nothing.** Data only reaches `scenes/` through
  `SceneStagingArea::commit`. Nothing writes there directly.
- **Data is durable before it is visible.** Files are flushed before the
  rename; the rename is flushed by the next commit or by `sync_scene_index`.
  A lost rename only means resume regenerates that scene, identically.
- **Existing scenes are never replaced.** `commit` fails with
  `DestinationExists` instead.
- **Resume requires an exact match.** `DatasetResumeIndex::open_or_create`
  refuses a dataset whose manifest differs in configuration bytes, asset
  catalog, backend, or scene count.
- **Output is deterministic.** No timestamps, stable JSON field order, and
  EXR blocks written in increasing line order. See
  [Determinism](../determinism.md).

## Changing it

- Any change to file contents changes dataset digests. Treat it as a format
  change: note it in the changelog, and update the pinned digests in the
  tests deliberately, not to make them pass.
- New file in a scene: add it to `required_artifacts` in `staging.rs` and to
  the checksum and validation code in `validation.rs`, then document it in
  [Storage](../storage.md).
- New exporter: read only from a committed dataset and the stored models,
  as `bop.rs` does. An exporter must never need the source assets or the
  configuration directory.

## Caveats

- Durability uses `F_FULLFSYNC` on macOS and `syncfs` on Linux; other
  platforms fall back to a full sync per file, which is much slower.
- The BOP export writes the `bin_id` of each instance, which is bin-picking
  specific. It is part of the scene model redesign.
- Datasets written before canonical models were stored cannot be exported;
  they have to be regenerated.

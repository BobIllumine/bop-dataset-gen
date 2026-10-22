# Codebase tour

This chapter follows one run through the code: from `bop-gen generate` to
scenes on disk, and then through the job server. Every step names the file
and function that does it, so you can read along. Links go to the
[code reference](../api/index.html), which includes private items.

Read [Architecture](../architecture.md) first for the picture; this chapter
is the same path at the level of functions.

## 1. The command line parses the request

`crates/bop-cli/src/main.rs`

- `parse` turns the arguments into a `Command`. `execute` dispatches it; for
  `generate` that is the function `generate`.
- `load_run` reads the configuration file, applies command-line `Overrides`
  to the TOML text, and calls [`LoadedRun::parse`].

[`LoadedRun`] keeps three things together: the exact configuration bytes, the
validated configuration, and the directory relative paths resolve against.
The bytes matter: the dataset manifest fingerprints them, so a resumed run
must present the same bytes, not just an equivalent configuration.

## 2. The configuration becomes a plan

`crates/bop-core/src/run_config/`

[`RunConfig::from_toml`] parses into private wire types (`wire.rs`, all
`deny_unknown_fields`), checks every rule (`validation.rs`), and produces a
[`RunPlan`]: validated values with no further checks needed. Everything
downstream takes a `RunPlan`, never raw TOML.

## 3. Assets are prepared once

`crates/bop-pipeline/src/asset_preparation.rs`, `crates/bop-assets/`

[`prepare_asset_catalog`] loads every configured STL or GLB, normalizes units
and axes, extracts materials and textures, and builds convex decompositions
for collision. The decompositions are cached on disk by content
(`bop-assets/src/convex_cache.rs`, `cache_key.rs`). The result,
[`PreparedAssetCatalog`], has two halves:

- the visual catalog (`ResolvedAssetCatalog`), which renderers receive, and
- the physics assets (`PreparedPhysicsAsset`), shared read-only by every
  physics worker through `Arc`.

## 4. A backend and a pipeline are chosen

`crates/bop-run/src/lib.rs`

[`bop_run::generate`] is the one place that names concrete renderers. It
builds one renderer per configured render worker with `renderer_pool`, then
picks the serial pipeline when there is one physics worker and one renderer,
and the parallel pipeline otherwise. The CLI and the server both call it, so
they always run the same pipeline.

## 5. The dataset is opened or resumed

`crates/bop-pipeline/src/serial_generation.rs`, `crates/bop-dataset/src/resume.rs`

`open_dataset_run` builds a [`DatasetManifest`] from the configuration bytes,
the asset catalog fingerprint, the backend's identity, and the scene count,
then calls [`DatasetResumeIndex::open_or_create`]. A new dataset gets
`run.json`, `resolved-config.toml`, `environment.json`, and the canonical
models under `assets/`. An existing one must match the manifest exactly, or
opening fails. The index lists the committed scenes; every other scene ID is
pending.

## 6. Physics produces a snapshot

`crates/bop-pipeline/src/parallel_physics.rs`, `crates/bop-physics-rapier/src/generator.rs`

In a parallel run, [`ParallelPhysicsExecutor`] starts one worker per
simulation worker. Each worker builds its own generator through
[`RapierSimulatorFactory`] and claims scene IDs from a shared atomic counter.
For each scene, [`SerialPhysicsGenerator::generate_scene`] tries attempts
until one is accepted:

1. Seeds for the population and placement stages are derived from the scene
   ID and attempt ID (see [Determinism](../determinism.md)).
2. The population planner chooses which assets appear, and how many.
3. The placement planner chooses starting poses inside the spawn volume.
   If they cannot fit, the attempt is rejected.
4. A fresh `PhysicsWorld` drops the bodies and settles them. If they do not
   settle within the step limit, the attempt is rejected.
5. The accepted attempt becomes a [`SceneSnapshot`]: final poses, the bins,
   and a physics report with rejection counts.

The snapshot is immutable and renderer-neutral. It goes into a bounded
channel, so physics can run at most `queue_capacity` scenes ahead of
rendering.

## 7. The snapshot becomes a render request

`crates/bop-pipeline/src/request_planner.rs`

A render worker takes a snapshot from the channel, and
[`RenderRequestPlanner::plan`] adds what rendering needs: every camera's
intrinsics and pose (shell cameras are sampled from the camera seed stage),
each instance's material (from the appearance stage), lights, and the
required channels. The result is a `RenderSceneRequest`. Camera and material
seeds are separate stages, so changing a camera never moves an object.

## 8. The scene is rendered into staging and committed

`crates/bop-pipeline/src/lib.rs`, `crates/bop-dataset/src/staging.rs`

[`SerialDatasetRenderer::render_and_commit`] is the transaction around one
scene:

1. Check that the renderer supports the requested channels and camera count.
2. Create a private staging directory with [`SceneStagingArea::create`] under
   `.staging/`.
3. Write `scene.json`, `materials.json`, and each camera's `frame.json`.
4. Call [`Renderer::render_scene`]. The backend writes `rgb.png`,
   `depth.exr`, and `instance.png` with the shared encoders in
   `bop-dataset/src/frames.rs`, so both backends produce the same formats.
5. Check the backend's report (`validate_report`), decode every frame, and
   write `checksums.json` (`validate_and_write_checksums`).
6. [`SceneStagingArea::commit`] makes every file durable, then renames the
   directory into `scenes/<scene id>`. The rename is the moment the scene
   exists. Any error before it drops the staging area, which deletes it.
7. Call the commit observer, if one is set. The server uses it to upload
   each scene as soon as it exists.

## 9. The run finishes

Each renderer is flushed, `sync_scene_index` makes the last renames durable,
and the pipeline returns a [`RunReport`]: scenes found already committed,
scenes generated now, the total, and whether the run was cancelled. The CLI
prints it and maps errors to exit codes with `GenerateError::exit_code`.

After that, `bop-gen validate` calls [`validate_dataset`] and
`bop-gen export bop` calls [`export_bop`]; both only read the dataset.

## The same run through the job server

`crates/bop-server/src/`

1. **Startup** (`lib.rs`, `build`): connect storage and the database, seed
   example assets into a new library, rebuild the asset index from storage,
   release jobs whose lease ran out, and build the router and the `Runner`.
2. **Submission** (`api.rs`, `create_job`): the `authorize` middleware checks
   the token. `submission::accept` checks the configuration, requires asset
   paths to be relative without `..` (they name files in the asset library),
   and replaces `[run] cache_path` with the server's cache. Missing assets are
   rejected before a job exists. BlenderProc paths in a submitted
   configuration are ignored; the server's own settings replace them. `JobStore::insert`
   records the job as queued.
3. **Claiming** (`runner.rs`, `Runner::run`): the runner loop claims the next
   queued job with a lease (`JobStore::claim_next`), and renews the lease in
   `hold_lease` while the job runs. One server runs one job at a time.
4. **Running** (`Runner::execute`): fetch the job's assets through the local
   asset cache, rebuild the local dataset from storage if another server
   started it, then run `bop_run::generate` on a blocking thread. The commit
   observer sends each committed scene's path back, and `upload_trees`
   uploads it right away.
5. **Finalizing** (`Runner::finalize`): validate the dataset, pack
   `dataset.tar` from the local copy, and run the BOP export and pack
   `bop.tar` when the job asked for it.

Storage is the source of truth for results; the database rows in
`bop_job_files`, `bop_assets`, and `bop_exports` are an index that can be
rebuilt from a storage listing (`index.rs`). See [Server internals](server.md).

[`LoadedRun`]: ../api/bop_run/struct.LoadedRun.html
[`LoadedRun::parse`]: ../api/bop_run/struct.LoadedRun.html#method.parse
[`RunConfig::from_toml`]: ../api/bop_core/struct.RunConfig.html#method.from_toml
[`RunPlan`]: ../api/bop_core/struct.RunPlan.html
[`prepare_asset_catalog`]: ../api/bop_pipeline/fn.prepare_asset_catalog.html
[`PreparedAssetCatalog`]: ../api/bop_pipeline/struct.PreparedAssetCatalog.html
[`bop_run::generate`]: ../api/bop_run/fn.generate.html
[`DatasetManifest`]: ../api/bop_dataset/struct.DatasetManifest.html
[`DatasetResumeIndex::open_or_create`]: ../api/bop_dataset/struct.DatasetResumeIndex.html#method.open_or_create
[`ParallelPhysicsExecutor`]: ../api/bop_pipeline/struct.ParallelPhysicsExecutor.html
[`RapierSimulatorFactory`]: ../api/bop_pipeline/struct.RapierSimulatorFactory.html
[`SerialPhysicsGenerator::generate_scene`]: ../api/bop_physics_rapier/struct.SerialPhysicsGenerator.html#method.generate_scene
[`SceneSnapshot`]: ../api/bop_core/struct.SceneSnapshot.html
[`RenderRequestPlanner::plan`]: ../api/bop_pipeline/struct.RenderRequestPlanner.html#method.plan
[`SerialDatasetRenderer::render_and_commit`]: ../api/bop_pipeline/struct.SerialDatasetRenderer.html#method.render_and_commit
[`SceneStagingArea::create`]: ../api/bop_dataset/struct.SceneStagingArea.html#method.create
[`SceneStagingArea::commit`]: ../api/bop_dataset/struct.SceneStagingArea.html#method.commit
[`Renderer::render_scene`]: ../api/bop_render_api/trait.Renderer.html#tymethod.render_scene
[`RunReport`]: ../api/bop_pipeline/struct.RunReport.html
[`validate_dataset`]: ../api/bop_dataset/fn.validate_dataset.html
[`export_bop`]: ../api/bop_dataset/fn.export_bop.html

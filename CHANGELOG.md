# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## 0.1.2

### Added

- **Example assets on a new deployment.** A new asset library gets the
  example meshes the studio's presets use (`cube.stl`, `hex_prism.stl`,
  `l_bracket.stl`, `textured_cube.glb`), once, without replacing files of the
  same name; `BOP_SEED_EXAMPLES=false` turns it off. Before, a fresh
  deployment, particularly on external S3, rejected every preset until the
  meshes were uploaded by hand.

- **GitHub Packages.** The server image is also published as
  `ghcr.io/bobillumine/bop-server`, and Docker Hub is optional. A manual run
  with a `tag` publishes an existing release again.
- **Port settings.** `deploy/.env.example` documents `BOP_PORT` and
  `BOP_STUDIO_PORT` for hosts where 8080 or 3000 is taken.
- **Storage index.** The server's database indexes the asset library, each
  job's stored files, and its export archives, so asset listings, the
  missing-asset check, job file listings, and export links no longer wait on
  storage. Storage stays the source of truth: the asset index is rebuilt at
  every start, and jobs from earlier versions are indexed on first use.
- **Asset cache.** Jobs fetch assets into `<data>/asset-cache`, keyed by
  entity tag, so a job reusing an asset does not download it again.
- **`BOP_STORAGE_CONCURRENCY`** (default 32): storage requests a job upload,
  asset download, or export keeps in flight.
- **Job leases.** Several servers can share one database: a running job is
  leased to its server (`BOP_RUNNER_ID`), and any server queues a job whose
  lease ran out again. The server that continues it rebuilds the local copy
  from storage, so committed scenes are not rendered again.
- **Cancel through any server.** A cancel request reaches the job whichever
  server runs it.
- **Live updates across servers.** On PostgreSQL, job changes are announced
  with `NOTIFY`: event streams follow jobs on every server, and idle servers
  start new jobs at once.
- **Local data retention.** `BOP_LOCAL_RETENTION_DAYS` (default 14) deletes
  local copies of failed and cancelled jobs, and cached copies of replaced
  assets, once they are that old.

### Changed

- **Faster storage transfers.** Result files, assets, and exports of older
  jobs move several at a time instead of one by one, and a resumed job only
  uploads the scenes storage does not hold yet.
- **Exports are packed as jobs finish.** `dataset.tar`, and `bop.tar` for
  jobs submitted with `export_bop`, are packed from the local copy before a
  job succeeds, so export links are ready at once. Requests for an archive
  being packed wait for it instead of packing it again.
- **The BOP export is stored only as `bop.tar`.** Job file listings no
  longer include `bop/...`; jobs from earlier versions keep theirs.
- **`BOP_REQUEUE_ON_START`** now only queues the jobs this server left
  running, so it is safe with several servers.
- **Run metadata is stored with the first scenes** instead of after the run.
- **Faster BOP export and validation.** `bop-gen export bop` and dataset
  validation process scenes in parallel: on 1,000 rendered 640×480 scenes the
  export takes 3.7 s instead of 22.9 s and validation 0.5 s instead of 3.2 s,
  with byte-identical output. Together with reading files ahead while packing
  archives, a server job's finishing step drops from about 31 s to about 5 s.
- **Faster encoding.** The wgpu renderer encodes a frame's RGB, depth, and
  instance images concurrently, and depth blocks are compressed in parallel;
  generation at the presets' 4 renderer workers is about 15% faster, with
  byte-identical output.
- **Depth is ZIP-compressed.** `depth.exr` is written as ZIP in 16-scanline
  blocks instead of tiled RLE, which barely compressed float depth: rendered
  depth takes 5–17% of its former size, and wgpu datasets shrink about 7×.
  The pixels are unchanged and every OpenEXR reader supports it. Older
  datasets keep their RLE files; bop-studio reads both.

### Fixed

- **Blender outlived failed jobs.** Stopping a BlenderProc renderer killed
  only the `blenderproc` launcher, leaving its Blender process running and
  holding GPU memory. The renderer now kills the worker's whole process
  group, and the compose server runs under an init that reaps leftovers.

## 0.1.1

### Changed

- **Determinism is per platform.** Datasets are byte-identical on the same
  platform (CPU architecture and operating system), in both the `fast` and
  `reproducible` builds; physics results differ in their last bits between,
  for example, x86_64 Linux and arm64 macOS. The guide no longer claims
  `reproducible` is cross-platform, and the corpus digest test pins each CI
  platform.

### Added

- **Deployment guide.** `docs/book/deployment.md` covers a GPU host with an
  existing authentik, external PostgreSQL and S3, and Tailscale Funnel.
- **Compose settings.** `BOP_SERVER_IMAGE` and `BOP_STUDIO_IMAGE` pull
  published images instead of building; `BOP_CONNECT_SRC` sets the studio's
  allowed API and sign-in origins.

### Fixed

- Tests that failed on x86_64: a transform-precision fixture that only held
  with glam's aarch64 NEON arithmetic, and the corpus digest.
- A physics-queue test that hung, instead of failing, on a slow machine.
- The Docker workflow's image name on builds without Docker Hub settings.

## 0.1.0

First release.

### Added

- **Deterministic generation.** One run seed fixes every scene: populations,
  placements, cameras, and appearance are derived from labelled per-stage
  seeds, independent of worker counts and scheduling.
- **Physics.** Rapier simulates objects dropped into analytic bins until they
  settle; invalid final states are rejected and replaced deterministically.
  Collision shapes come from cached convex decompositions.
- **Assets.** STL and glTF/GLB meshes with unit scaling and canonicalization.
  `bop-gen optimize-asset` simplifies a mesh within a measured error budget and
  writes an optimized GLB beside it.
- **wgpu renderer.** A headless renderer for RGB, depth, and instance masks:
  metallic-roughness PBR with base-color textures, shadow mapping, ambient
  occlusion, HDR environment maps, optional screen-space reflections, and
  optional FXAA.
- **BlenderProc renderer.** A supervised, persistent BlenderProc 2.8 worker
  (Blender 4.2 LTS) on the same scene snapshots, with configurable Cycles
  sampling, adaptive noise threshold, and OIDN or OptiX denoising.
- **Materials.** Per-asset visual overrides, defaults for meshes without a
  material, and bin render materials, sampled per instance and recorded in each
  scene's `materials.json`.
- **Multiple GPUs.** Renderer workers spread over equivalent GPUs, one GPU per
  worker in turn, for both backends.
- **Datasets.** Isolated staging, checksums, validation, and atomic
  publication of complete scenes; group commit; compatible resume after an
  interruption; configurable PNG compression.
- **BOP export.** `bop-gen export bop` converts a dataset to the BOP format.
- **Command line.** `bop-gen` with `doctor`, `schema`, `prepare-assets`,
  `simulate`, `render`, `generate`, `validate`, `export bop`, `bench`, and
  `optimize-asset`, plus a documented exit-code contract.
- **Benchmarks.** `bop-gen bench` renders one snapshot corpus with each backend
  and reports throughput, stage timings, memory, and cross-backend image
  agreement.
- **Job server.** `bop-server` runs generation jobs over HTTP with an OpenAPI
  contract, job records in SQLite or PostgreSQL, results in S3-compatible
  storage, and Docker deployment files. BOP export is always included.
- **Sign-in.** Optional OIDC access tokens (authentik or any provider with
  asymmetrically signed JWTs), checked for signature, issuer, audience,
  expiry, and optionally group, alongside the shared `BOP_API_TOKEN`. An
  `authentik.yaml` compose overlay runs a preconfigured authentik. The server
  refuses to serve a non-loopback address without credentials unless
  `BOP_ALLOW_ANONYMOUS=true`.
- **Documentation.** A developer guide in `docs/book` and a JSON Schema for run
  configurations.

### Known limitations

- On Metal, GPU timestamp queries can report no GPU time, so `gpu_seconds` is
  missing from some wgpu reports.
- wgpu's environment lighting is only partly occluded by ambient occlusion, so
  crevices are somewhat brighter than in BlenderProc.
- wgpu images are not pixel-identical across GPU models.
- Throughput on Linux with NVIDIA GPUs, and the Docker images, have not yet been
  measured on hardware.

# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

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

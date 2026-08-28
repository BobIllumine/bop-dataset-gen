# bpp-dataset-gen

Deterministic synthetic bin-picking dataset generation in Rust. Rapier owns
scene physics; interchangeable `wgpu` and BlenderProc backends consume the same
immutable scene and camera snapshots. Renderers write only to isolated staging
trees, and the dataset layer validates and atomically publishes complete scenes.

The implementation is intentionally split into renderer-neutral crates. Start
with [the renderer guide](docs/book/renderers.md) for coordinate conventions,
runtime setup, output contracts, verification, and current limitations.

Local development uses Rust 1.97.1. The repository-local `superpowers/`
directory is planning material and remains ignored by Git.

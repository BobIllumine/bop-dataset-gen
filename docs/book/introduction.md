# Introduction

`bop-dataset-gen` generates synthetic training data for robotic bin picking.
It drops 3D objects into a virtual bin, lets Rapier settle them, renders every
settled scene from one or more cameras, and saves RGB, metric depth, visible
instance masks, and exact object poses.

Two properties drive most of the design:

- **Reproducible.** The same configuration produces byte-identical output, no
  matter how many workers run or in which order scenes finish.
- **Crash-safe.** A run can be interrupted at any point and resumed. A scene is
  either completely published or absent; it is never half written.

## Quick start

```text
cargo run -- schema > run-config.schema.json   # every configuration field
cargo run -- doctor --renderer wgpu            # is a GPU adapter available?
cargo run -- generate --config run.toml --renderer wgpu --output dataset
cargo run -- validate dataset
```

The fastest way to watch a complete run is the real-GPU smoke test. It writes
a small asset and configuration, generates a dataset, validates it, then
corrupts one file and checks that validation catches it:

```text
cargo test -p bop-cli --bin bop-gen -- --ignored real_wgpu_generate_cli
```

## Where to read next

Start with [Architecture](architecture.md) for how the crates fit together,
then [Determinism and seeds](determinism.md) and [Concurrency](concurrency.md),
which explain the guarantees above.

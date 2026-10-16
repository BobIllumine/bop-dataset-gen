# Development and CI

## Local checks

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --features bpp-cli/blenderproc -- -D warnings
cargo test --workspace
cargo run -p xtask -- schema --check
cargo deny check
mdbook build docs
```

The `reproducible` feature set is checked separately:

```text
cargo test -p bpp-cli -p bpp-pipeline -p bpp-physics-rapier --no-default-features \
  --features bpp-cli/reproducible,bpp-cli/wgpu,bpp-cli/blenderproc,bpp-pipeline/reproducible,bpp-physics-rapier/reproducible
```

## Hardware tests

Tests that need a GPU or Blender are `#[ignore]`d. Run them explicitly:

```text
cargo test -p bpp-render-wgpu -- --ignored
cargo test -p bpp-cli --bin bpp -- --ignored real_wgpu_generate_cli
cargo test -p bpp-render-blenderproc --test worker_supervision -- --ignored real_worker
```

## Workflows

| Workflow | Runs |
|---|---|
| `ci.yml` | On every push and pull request: formatting, clippy and tests on Linux and macOS for both feature sets, rustdoc, schema drift, the mdBook, cargo-deny, coverage, and the Python worker checks. One final job, "CI passed", succeeds only if all others did. |
| `hardware.yml` | Weekly and on demand: the ignored wgpu tests (lavapipe or a self-hosted Mac) and BlenderProc tests. Each job first runs `bpp doctor`, so a missing runtime fails instead of skipping. |
| `release.yml` | On a `v*` tag: checks the tag against the workspace version, reruns CI, builds Linux and macOS archives in both feature sets, and publishes a GitHub release with checksums. |

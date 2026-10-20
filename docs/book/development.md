# Development and CI

## Local checks

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets --features bop-cli/blenderproc -- -D warnings
cargo test --workspace
cargo run -p xtask -- schema --check
cargo deny check
mdbook build docs
```

`cargo test --workspace` includes the API contract check: it fails when
`schemas/api.openapi.json` no longer matches the server's routes. After an
intended API change, regenerate it and commit the result:

```text
BOP_BLESS=1 cargo test -p bop-server --test openapi
```

The `reproducible` feature set is checked separately:

```text
cargo test -p bop-cli -p bop-pipeline -p bop-physics-rapier --no-default-features \
  --features bop-cli/reproducible,bop-cli/wgpu,bop-cli/blenderproc,bop-pipeline/reproducible,bop-physics-rapier/reproducible
```

## Hardware tests

Tests that need a GPU or Blender are `#[ignore]`d. Run them explicitly:

```text
cargo test -p bop-render-wgpu -- --ignored
cargo test -p bop-cli --bin bop-gen -- --ignored real_wgpu_generate_cli
cargo test -p bop-render-blenderproc --test worker_supervision -- --ignored real_worker
```

## Workflows

| Workflow | Runs |
|---|---|
| `ci.yml` | On every push and pull request: formatting, clippy and tests on Linux and macOS for both feature sets, rustdoc, schema drift, the mdBook, cargo-deny, coverage, the Python worker checks, and a job-server run against PostgreSQL and MinIO on lavapipe (`cargo test -p bop-server --test end_to_end -- --ignored`; without the `BOP_TEST_*` variables it uses SQLite and a local directory). One final job, "CI passed", succeeds only if all others did. |
| `hardware.yml` | Weekly and on demand: the ignored wgpu tests (lavapipe or a self-hosted Mac) and BlenderProc tests. Each job first runs `bop-gen doctor`, so a missing runtime fails instead of skipping. |
| `docker.yml` | After CI passes on a push to `master`: builds `deploy/Dockerfile` and pushes `latest` and `sha-<commit>` to GitHub Packages (`ghcr.io/<owner>/bop-server`) and, when configured, Docker Hub. A `v*` tag reruns CI and pushes the version. Pull requests that change the image build it without pushing. `gh workflow run docker.yml -f tag=v0.1.1` publishes an existing tag again. Docker Hub needs the `DOCKERHUB_USERNAME` variable and `DOCKERHUB_TOKEN` secret, plus `DOCKERHUB_IMAGE` to override the default `<user>/bop-server`. |
| `release.yml` | On a `v*` tag: checks the tag against the workspace version, reruns CI, builds Linux and macOS archives in both feature sets, and publishes a GitHub release with checksums. |

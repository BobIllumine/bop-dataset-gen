# bop-dataset-gen

> ## DISCLAIMER
> This project was created with heavy AI assistance.
> Not proud of that, but it had to be done for one of the tasks I'm working on.
> I will probably continue working on this project, although not as intensively.
> If you have any suggestions or bugs appearing, leave an issue or a PR please.


Deterministic synthetic bin-picking dataset generation in Rust. Rapier owns
scene physics; interchangeable `wgpu` and BlenderProc backends consume the same
immutable scene and camera snapshots. Renderers write only to isolated staging
trees, and the dataset layer validates and atomically publishes complete scenes.

The implementation is intentionally split into renderer-neutral crates. The
developer guide in [`docs/book`](docs/book/SUMMARY.md) covers the architecture,
configuration, determinism, concurrency, storage, renderers, and the CLI.
Build it with `mdbook build docs` (output in `target/book`). The guide and the
code reference are published at <https://bobillumine.github.io/bop-dataset-gen/>.

Generate, validate, and export the eight-scene smoke preset:

```text
cargo run --release -p bop-cli -- generate --config examples/presets/smoke.toml --renderer wgpu --output out/smoke
cargo run --release -p bop-cli -- validate out/smoke
cargo run --release -p bop-cli -- export bop out/smoke --output out/smoke-bop
```

To run jobs over HTTP, with job records in SQLite or PostgreSQL and results in
S3-compatible storage, see the [job server](docs/book/server.md) guide and
`deploy/compose.yaml`.

Local development uses Rust 1.97.1. The repository-local `superpowers/`
directory is planning material and remains ignored by Git.

## License

Copyright (C) 2026 Robert Chen

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. See [LICENSE](LICENSE).

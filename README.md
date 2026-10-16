# bpp-dataset-gen

Deterministic synthetic bin-picking dataset generation in Rust. Rapier owns
scene physics; interchangeable `wgpu` and BlenderProc backends consume the same
immutable scene and camera snapshots. Renderers write only to isolated staging
trees, and the dataset layer validates and atomically publishes complete scenes.

The implementation is intentionally split into renderer-neutral crates. The
developer guide in [`docs/book`](docs/book/SUMMARY.md) covers the architecture,
configuration, determinism, concurrency, storage, renderers, and the CLI.
Build it with `mdbook build docs` (output in `target/book`).

Local development uses Rust 1.97.1. The repository-local `superpowers/`
directory is planning material and remains ignored by Git.

## License

Copyright (C) 2026 Robert Chen

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. See [LICENSE](LICENSE).

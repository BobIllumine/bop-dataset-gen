# Determinism and seeds

The goal: the same resolved configuration produces byte-identical output on
the same build and platform, regardless of worker counts or the order in which
scenes finish.

*Platform* means CPU architecture and operating system, such as x86_64 Linux
or arm64 macOS. Floating-point results differ in their last bits between
platforms (glam's SIMD paths, Rapier, and the system math library each round
differently), and those bits change where objects settle. A dataset generated
on another platform is equally valid, but not byte-identical. Generate a
dataset, and resume it, on one platform.

## Scenes are addressed, not sequenced

A run has scene IDs `0..valid_scene_count`. Nothing about a scene depends on
which scenes ran before it, so any scene can be generated alone, in any order,
on any worker. This is what makes parallelism and resume safe.

## Seed derivation

`SeedDeriver` (in `bop-core`) turns a master seed into independent streams:

1. The master seed is encoded as 8 little-endian bytes and passed to BLAKE3's
   key-derivation function with the context `bpp.dataset.seed-master.v1`.
   The result is a private 32-byte key.
2. Each stream seed is the keyed BLAKE3 hash of: the scene ID (8 bytes, little
   endian), the attempt ID (4 bytes, little endian), and a fixed stage label
   such as `bpp.dataset.seed-stage.v1/placement`.

Stages are `population`, `placement`, `camera`, and `appearance`. Because each
stage has its own stream, changing a camera setting cannot change where
objects land. The protocol is pinned by known test vectors in
`crates/bop-core/tests/seed_derivation.rs`; changing any label or encoding is
a protocol version change.

## Attempts and rejection

A scene can be *rejected*: objects did not fit in the spawn volume, or the pile
did not settle within the step limit. Rejection is not an error. The generator
retries with the next attempt ID, which selects fresh streams, up to
`scene_attempt_limit`. The accepted attempt and the count of each rejection
reason are recorded in the snapshot. If every attempt is rejected, the run
fails with exit code 4.

## Byte-level reproducibility

Matching inputs is not enough; every encoder must be deterministic too.
Known sources of nondeterminism are handled explicitly:

- **Physics features.** The default `fast` feature uses SIMD Rapier. The
  `reproducible` feature enables Rapier's enhanced determinism, which removes
  Rapier's own run-to-run variation but does not make other float math match
  across platforms. The two give different output, are mutually exclusive, and
  the enabled feature is recorded in `environment.json`. A test pins an
  eight-scene corpus digest for each feature on each CI platform
  (`crates/bop-physics-rapier/tests/serial_generator.rs`).
- **Depth EXR.** The EXR writer encodes sequentially. Its default parallel mode
  writes compressed chunks in completion order, which changes the file bytes
  between runs even though the pixels match.
- **JSON.** Metadata is serialized from typed structures with stable field
  order and no timestamps.

Tests compare serial and parallel output byte for byte. If you add an encoder,
run a configuration twice and diff the outputs.

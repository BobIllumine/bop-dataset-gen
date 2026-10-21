# Storage, durability, and resume

## Dataset layout

```text
dataset/
├── run.json               manifest: config fingerprint, asset catalog, backend, scene count
├── resolved-config.toml   the exact configuration bytes of this run
├── environment.json       generator version, OS, architecture, profile, features
├── assets/
│   └── <asset-id>/        asset ID, with unsafe bytes percent-encoded
│       ├── asset.json     asset ID, content fingerprint, mesh checksum and counts
│       └── model.ply      the canonical visual mesh, model-frame metres
├── .staging/              in-progress scenes; never part of the dataset
└── scenes/
    └── 00000000000000000000/      scene ID, 20 digits
        ├── scene.json             the scene snapshot
        ├── materials.json         the surface each object was rendered with
        ├── checksums.json         BLAKE3 of every file below
        └── frames/
            └── 00001/             camera ID, 5 digits
                ├── frame.json     camera intrinsics and extrinsics
                ├── rgb.png        8-bit sRGB
                ├── depth.exr      32-bit float camera-Z metres, ZIP in 16-line blocks
                └── instance.png   16-bit visible instance IDs, 0 = background
```

The `assets/` models are the exact meshes the scenes were rendered with, so
exporters such as [BOP export](bop.md) never need the original source files.
They are written once when a run starts. A resumed run must present
byte-identical models, which the asset catalog fingerprint in `run.json`
already guarantees. Datasets written before model storage existed cannot be
exported; regenerate them.

A snapshot corpus written by `bop-gen simulate` has the same shape with
`corpus.json` as its manifest and only `scene.json` in each scene directory.

## Atomic commit

A renderer writes into `.staging/<scene>`. To commit, the dataset layer checks
that every required artifact exists and is non-empty, decodes each frame to
validate it, writes `checksums.json`, makes the staged files durable, and then
renames the directory into `scenes/`. The rename is atomic, so a reader sees a
whole scene or none of it.

## Group commit

Making data durable means flushing the storage device's cache, which is slow
(about 4 ms on macOS) and serialized by the device. `bop-dataset` therefore
makes many files durable with one flush:

1. Hand every staged file and directory to the device without the expensive
   cache flush: a plain `fsync` on macOS; on Linux this is deferred.
2. Issue one barrier that covers all of them: `F_FULLFSYNC` on macOS,
   `syncfs` on Linux. Other platforms fall back to a full sync per file.
3. Rename the scene directories into place.

The snapshot writer drains up to 64 queued snapshots and commits them under a
single barrier.

What this guarantees after a power failure:

- A scene's data is always durable before its rename, so a published scene is
  never partial.
- A rename becomes durable with the next barrier, or with the final index sync
  every run performs on completion. A rename lost to power failure only means
  that scene is missing, and resume regenerates it identically.

## Resume

`--resume` reopens a dataset only if its manifest matches the requested run
exactly: resolved configuration, asset catalog, renderer provenance, and scene
count. Committed scenes are found by scanning `scenes/`; everything else is
pending. Leftover staging directories are ignored.

`bop-gen validate DATASET` re-reads every committed scene, decodes its frames,
and verifies all checksums. It exits with code 5 on any mismatch.

# Storage, durability, and resume

## Dataset layout

```text
dataset/
├── run.json               manifest: config fingerprint, asset catalog, backend, scene count
├── resolved-config.toml   the exact configuration bytes of this run
├── environment.json       generator version, OS, architecture, profile, features
├── .staging/              in-progress scenes; never part of the dataset
└── scenes/
    └── 00000000000000000000/      scene ID, 20 digits
        ├── scene.json             the scene snapshot
        ├── checksums.json         BLAKE3 of every file below
        └── frames/
            └── 00001/             camera ID, 5 digits
                ├── frame.json     camera intrinsics and extrinsics
                ├── rgb.png        8-bit sRGB
                ├── depth.exr      32-bit float camera-Z metres
                └── instance.png   16-bit visible instance IDs, 0 = background
```

A snapshot corpus written by `bpp simulate` has the same shape with
`corpus.json` as its manifest and only `scene.json` in each scene directory.

## Atomic commit

A renderer writes into `.staging/<scene>`. To commit, the dataset layer checks
that every required artifact exists and is non-empty, decodes each frame to
validate it, writes `checksums.json`, makes the staged files durable, and then
renames the directory into `scenes/`. The rename is atomic, so a reader sees a
whole scene or none of it.

## Group commit

Making data durable means flushing the storage device's cache, which is slow
(about 4 ms on macOS) and serialized by the device. `bpp-dataset` therefore
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

`bpp validate DATASET` re-reads every committed scene, decodes its frames,
and verifies all checksums. It exits with code 5 on any mismatch.

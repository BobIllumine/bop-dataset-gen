# Architecture rules

These rules keep the codebase understandable as it grows. They apply to
every change, whoever or whatever wrote it. A review checks a change against
them; a change that has to break one says so in its description and explains
why.

Some are enforced by tools. The rest are enforced by review, and are marked
that way.

## Boundaries

1. **Dependencies point one way.** A crate only uses the crates listed above
   it in [Crates](crates.md#allowed-dependencies). *Review.*
2. **Stages talk through data, not calls.** Physics produces a
   `SceneSnapshot`; renderers consume a `RenderSceneRequest`. Neither knows
   the other exists. *Review.*
3. **One place chooses a backend.** Only `bop-run` (and `bop-gen render`,
   until it moves there) names concrete renderers. `bop-pipeline` stays
   generic over `Renderer`. *Review.*
4. **One owner per format.** Every file the generator writes is encoded and
   validated in `bop-dataset`. Renderers and exporters use its encoders
   instead of writing their own. *Review.*
5. **The server adds service concerns only.** Authentication, persistence,
   leases, uploads. Anything a command-line run would also need belongs in
   `bop-run` or below. *Review.*

## Data and configuration

6. **Configuration is typed.** Every field is a Rust type with
   `deny_unknown_fields`; choices are enums, not strings compared at run
   time; limits are checked once in validation and then trusted. No maps
   from string keys to behaviour, and no configuration entries that only
   repeat their own name. *Review.*
7. **Validated types stay valid.** Types such as `PositiveMeters` or
   `SceneSnapshot` check their invariants in a constructor (`try_new`) and
   keep their fields private, so holding a value proves it is valid.
   *Review.*
8. **Every persisted structure has a `schema_version`.** Reading an old
   version either works or fails with a clear error; it never misreads.
   *Review; serialization tests.*

## Determinism and durability

9. **Same input, same bytes.** No wall-clock time, randomness without a
   derived seed, or `HashMap` iteration order may reach output. Each seeded
   decision has its own `SeedStage`. *Tests pin digests per platform.*
10. **Frozen labels stay frozen.** The `bpp.*` seed and fingerprint domains
    and the `bpp-*` cache-key labels predate the rename to `bop` and are
    inputs to hashes. Renaming them changes every seed, fingerprint, and
    cache key. *Known-vector tests.*
11. **Output changes are format changes.** If a change alters dataset bytes,
    it says so in the changelog, and pinned digests are updated on purpose,
    never just to make a test pass. *Review.*
12. **Publish atomically.** Scenes reach `scenes/` only through
    `SceneStagingArea::commit`. Server index rows are written after their
    object and deleted before it. *Review; commit tests.*

## Code shape

13. **One responsibility per module.** A module's `//!` comment says what it
    owns in a sentence or two; if it needs an "and also", split it.
    *Review.*
14. **Size limits.** Keep a source file under about 600 lines of code,
    not counting its `#[cfg(test)]` module. Over 800 is a split, not a
    discussion. Functions that need `#[allow(clippy::too_many_lines)]` are
    split instead. *Review.* Files over 800 lines today, due to be split:
    `bop-render-wgpu/src/lib.rs` (about 1,800), `bop-assets/src/glb.rs`,
    `bop-server/src/api.rs`, `bop-core/src/scenes.rs`,
    `bop-cli/src/main.rs`, `bop-server/src/db.rs`,
    `bop-render-blenderproc/src/lib.rs`, `bop-server/src/runner.rs`,
    `bop-dataset/src/bop.rs`, `bop-physics-rapier/src/world.rs`, and
    `bop-core/src/run_config/wire.rs`.
15. **Errors are typed.** Each crate has `thiserror` error enums. Library
    code does not panic on bad input; `expect` is only for invariants, and
    its message says why it cannot fail. Generation failures map to exit
    codes in one place (`GenerateError::exit_code`), and every exit code is
    listed in [Command line](../cli.md#exit-codes). *Clippy; review;
    `bop-cli/tests/exit_codes.rs`.*
16. **Lint exceptions carry a reason.** The workspace denies all of Clippy's
    default lints and warns on pedantic ones. A new `#[allow(...)]` is as
    narrow as possible and includes `reason = "..."`. *Clippy; review.*
17. **No unsafe code.** *`unsafe_code = "forbid"`.*

## Documentation

18. **Everything public is documented.** *`missing_docs = "deny"`.*
19. **Every crate and module starts with an overview.** What it owns, what it
    must not know about, and where to start reading. *Review.*
20. **Docs change with the code.** A change to behaviour, configuration,
    formats, or the API updates the guide chapter that describes it, in the
    same pull request. *Review.*
21. **The API contract is generated.** `schemas/api.openapi.json` is produced
    from the routes and checked in CI. *`tests/openapi.rs`.*

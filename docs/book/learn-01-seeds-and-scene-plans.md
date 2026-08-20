# LEARN-01: Deterministic seeds and scene plans

LEARN-01 asks you to complete two small algorithms that define how BPP plans
reproducible work. The surrounding public API, protocol labels, validation,
documentation, and executable contract are already present so you can focus on
Rust ownership and state transitions.

The learner-owned bodies are:

- `SeedDeriver::new` and `SeedDeriver::derive` in
  `crates/bpp-core/src/deterministic_planning.rs`;
- `ScenePlanIter::next` in the same file.

Do not redesign the public signatures, protocol strings, iterator order, or
test vectors while completing the exercise.

## Dependency status

The implementation is intended to use the official `blake3` crate as a normal
`bpp-core` dependency. On 2026-08-20, BLAKE3 1.8.6 still depended on
`arrayref ^0.3.5` while every matching `arrayref` release was yanked. Cargo
therefore refused to create a new safe lock entry. The scaffold deliberately
does not vendor, patch, or lock a yanked release.

Wait for BLAKE3 to publish a dependency graph Cargo can resolve without yanked
packages, then add the official `blake3` crate to `[dependencies]` in
`crates/bpp-core/Cargo.toml` before implementing the seed methods. Do not work
around this with a git dependency, local vendor copy, or manually seeded yanked
lock entry.

## Concepts practiced

- explicit little-endian, fixed-width protocol encoding;
- BLAKE3 key derivation and keyed hashing with domain separation;
- immutable owned state that is naturally `Send + Sync`;
- deterministic results that do not depend on worker scheduling;
- a finite iterator expressed as a small, nonrecursive state machine;
- nonzero bounds and overflow reasoning across full identifier domains.

The master seed becomes private key material through BLAKE3's key-derivation
API with context `bpp.dataset.seed-master.v1`. A stream is selected by the
fixed-width scene bytes, fixed-width attempt bytes, and one of the public
versioned stage labels. The complete byte contract is documented on
`SeedDeriver` and `SeedStage`; the expected outputs are intentionally kept in
`crates/bpp-core/tests/learn_01.rs`.

The scene plan is scene-major: emit every attempt for scene zero, then every
attempt for scene one, and so on. A `2 x 3` plan therefore exposes six jobs in
lexicographic `(scene, attempt)` order.

## Recommended implementation order

1. Confirm that Cargo can safely resolve the official `blake3` dependency.
2. Complete `SeedDeriver::new`, then run the ignored exercise command. Its
   failure should move from construction to derivation.
3. Complete `SeedDeriver::derive`. Check the stable vectors first, then the
   repeatability, tuple-separation, and scoped-thread assertions.
4. Before editing `ScenePlanIter::next`, write a state table for an ordinary
   attempt, the final attempt of a scene, the final scene, and repeated calls
   after exhaustion.
5. Complete `next` and verify the exact sequence, permanent exhaustion,
   independent-iterator, and large-bound assertions.
6. Run the focused and workspace quality gates listed below.

Keep `size_hint` and `ExactSizeIterator` out of the first solution. A total job
count can exceed `usize`, and neither optimization is needed for correct
iteration.

## Commands

From the repository root, the exercise checkpoint is:

```console
cargo test -p bpp-core --test learn_01 -- --ignored
```

Before the learner bodies are complete, this command is expected to fail at a
`LEARN-01` TODO. Ordinary tests deliberately skip that exercise:

```console
cargo test -p bpp-core --test learn_01 --locked
cargo test --workspace --locked
```

After all three bodies are complete, also run:

```console
cargo test -p bpp-core --all-features --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo run -p xtask --locked -- schema --check
git diff --check
```

## Debugging checklist

- Read the first panic or assertion; do not change expected bytes to match an
  implementation.
- Check the KDF context and every stage label byte for exact spelling and
  version.
- Check that the master seed, scene ID, and attempt ID use explicit
  little-endian conversion with widths `u64`, `u64`, and `u32` respectively.
- Check that the stream input has no debug text, decimal conversion,
  native-endian values, separators, or hidden mutable state.
- Check the state table rather than adding special-case recursion.
- Check the one-scene/one-attempt case and call `next` more than once after
  exhaustion.
- Check large bounds without multiplying them into a `usize` length.
- If a thread-order assertion differs, look for a mutable RNG cursor or global
  state; scheduling is not part of the seed tuple.

## Ownership and parallelism traps

`SeedDeriver` owns 32 private bytes and never changes them after construction.
Its `derive(&self, ...)` interface allows many threads to read the same value
without a mutable borrow. Adding a mutex, atomics, a global RNG, or a shared RNG
cursor would make results depend on call order and defeat the protocol.

`ScenePlanIter` is different: iteration advances private counters through
`&mut self`. Give each planner its own iterator and send immutable `SceneJob`
values to workers. Do not put the iterator behind `Arc<Mutex<_>>`; that would
couple job assignment to scheduling even if it happened to remain data-race
free. No Rayon integration is needed in `bpp-core` for this exercise.

## Completion criteria

- all three `LEARN-01` TODO bodies are replaced without changing their public
  signatures;
- the ignored exercise command passes every stable-vector, separation,
  thread-order, iterator-order, exhaustion, independence, validation, and
  large-bound assertion;
- ordinary focused and workspace gates remain green;
- no global RNG, mutable seed cursor, lock, eager job list, recursive `next`,
  native-endian encoding, debug formatting, or total-length multiplication is
  introduced.

## References

- [BLAKE3 Rust implementation and documentation](https://github.com/BLAKE3-team/BLAKE3)
- [`blake3::derive_key`](https://docs.rs/blake3/latest/blake3/fn.derive_key.html)
- [BLAKE3 keyed `Hasher`](https://docs.rs/blake3/latest/blake3/struct.Hasher.html#method.new_keyed)
- [`u64::to_le_bytes`](https://doc.rust-lang.org/std/primitive.u64.html#method.to_le_bytes)
- [`Iterator`](https://doc.rust-lang.org/std/iter/trait.Iterator.html)
- [`FusedIterator`](https://doc.rust-lang.org/std/iter/trait.FusedIterator.html)
- [Rust Book: shared-state concurrency](https://doc.rust-lang.org/book/ch16-03-shared-state.html)

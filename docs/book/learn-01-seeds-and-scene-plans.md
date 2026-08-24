# LEARN-01: Deterministic seeds and scene plans

LEARN-01 implemented two small algorithms that define how BPP plans
reproducible work. The surrounding public API, protocol labels, validation,
documentation, and executable contract isolate the relevant Rust ownership and
state-transition concepts.

The learner-owned bodies are:

- `SeedDeriver::new` and `SeedDeriver::derive` in
  `crates/bpp-core/src/deterministic_planning.rs`;
- `ScenePlanIter::next` in the same file.

Do not redesign the public signatures, protocol strings, iterator order, or
test vectors while completing the exercise.

## Dependency status

The official `blake3` crate is an exact-pinned normal `bpp-core` dependency.
Use that dependency directly when implementing the seed methods; do not add a
git dependency, local vendor copy, or an alternative hashing implementation.

BLAKE3 1.8.7 removed its former `arrayref` dependency after the crates.io
owner of `arrayref` was compromised. The workspace deliberately pins 1.8.7 so
the LEARN-01 implementation uses the reviewed dependency graph that contains
neither `arrayref` nor the malicious `proc-macro1` crate.

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

## Implementation sequence

1. Inspect the official `blake3` APIs linked below and derive the private key
   material in `SeedDeriver::new`.
2. Implement `SeedDeriver::derive`. Check the stable vectors first, then the
   repeatability, tuple-separation, and scoped-thread assertions.
3. For `ScenePlanIter::next`, use a state table covering an ordinary
   attempt, the final attempt of a scene, the final scene, and repeated calls
   after exhaustion.
4. Verify the exact sequence, permanent exhaustion,
   independent-iterator, and large-bound assertions.
5. Run the focused and workspace quality gates listed below.

Keep `size_hint` and `ExactSizeIterator` out of the first solution. A total job
count can exceed `usize`, and neither optimization is needed for correct
iteration.

## Commands

From the repository root, run the focused regression contract with:

```console
cargo test -p bpp-core --test learn_01 --locked
```

The completed exercise tests are ordinary regression tests and therefore also
run as part of `cargo test --workspace --locked`. The complete quality gates
are:

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
- the focused regression command passes every stable-vector, separation,
  thread-order, iterator-order, exhaustion, independence, and large-bound
  assertion;
- the ordinary scaffold test passes its public-API, trait, fixed-label,
  constructor-validation, and maximum-bound construction assertions;
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

# LEARN-02: bounded parallel physics

Implement only `ParallelPhysicsExecutor::run` in
`crates/bpp-pipeline/src/parallel_physics.rs`. The serial pipeline is the
behavioral oracle. Do not share a Rapier world between workers.

## Required algorithm

1. Return immediately with a cancelled, zero-count report if cancellation is
   already set. Do not create a worker or allocate a scene ID.
2. Build one dedicated Rayon thread pool containing exactly `workers` threads.
3. Create exactly one simulator per worker through `SimulatorFactory::create`.
4. Allocate scene IDs atomically from `0..plan.run().valid_scene_count()`.
5. Check cancellation before claiming an ID and again before starting its
   simulation. An already claimed scene may finish.
6. Call `PhysicsSimulator::simulate` with the claimed ID. The simulator owns
   deterministic retry attempts; the executor must not invent retries.
7. Send accepted snapshots through the supplied bounded sender. Do not hold a
   mutex or allocator guard while `send` blocks.
8. Treat receiver drop as the first fatal error, request cancellation, and stop
   allocating IDs.
9. Catch worker panics at the task boundary, preserve the first fatal error,
   request cancellation, and still join the entire pool.
10. Drop every sender clone before returning so the receiver eventually
    disconnects. Return the number of successful sends, not claimed IDs.

The output order may vary. Dataset identity must not: scene IDs and seed
derivation address each scene independently, and downstream publication sorts
or indexes by `SceneId`.

## Ownership strategy

- Share only the factory, cancellation token, atomic counters, and a small
  first-error slot.
- Move each simulator into one long-lived worker closure.
- Clone the channel sender once per worker and drop the original before join.
- Never put a simulator or Rapier world behind `Arc<Mutex<_>>`.

## Verification

Run the ordinary interface test first:

```text
cargo test -p bpp-pipeline --test learn_02 --locked
```

Then run the exercise harness:

```text
cargo test -p bpp-pipeline --test learn_02 --locked -- --ignored --nocapture
```

The ignored harness currently stops at the LEARN-02 `todo!`. Completion also
requires tests for backpressure, early receiver drop, simulator failure,
worker panic, cancellation, and identical scene-ID sets across worker counts.

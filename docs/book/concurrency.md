# Concurrency

Parallel runs have two stages joined by a bounded channel:

```text
 physics workers (Rayon pool)          renderer workers (one thread each)
 ┌──────────┐                          ┌────────────┐
 │ Rapier 1 │──┐                    ┌─►│ renderer 1 │──┐
 │ Rapier 2 │──┼──► bounded queue ──┼─►│ renderer 2 │──┼──► atomic commits
 │   ...    │──┘   (queue_capacity) └─►│    ...     │──┘
 └──────────┘                          └────────────┘
```

## Parallel physics

`ParallelPhysicsExecutor` follows these rules:

- **One world per worker.** A `SimulatorFactory` builds an independent Rapier
  generator for each worker. Prepared collision assets are shared read-only
  through `Arc`; no physics world is ever shared.
- **Each scene exactly once.** Workers claim scene IDs from one atomic counter.
- **Bounded memory.** Snapshots go through a bounded channel. When it is full,
  a worker blocks in `send`, so at most `queue_capacity + simulation_workers`
  snapshots exist at once. No lock is held while blocking.
- **Cancellation.** Workers check the cancellation token before claiming a
  scene and again before simulating it. A scene already being simulated
  finishes.
- **Failure.** The first fatal error wins. A simulator error, a worker panic
  (caught at the task boundary), or a dropped receiver cancels the token so
  the rest of the pipeline stops, and every worker is joined before `run`
  returns.

## Parallel rendering

The render pool gives each renderer its own thread and a clone of the
channel's receiver. Commits are atomic and keyed by scene ID, so their order
does not matter. The first render failure stops the other renderers before
their next scene; dropping the last receiver then stops physics at its next
send.

## Why throughput did not scale at first

Measured on an M4 Pro, physics alone scales almost linearly: 8.3x on 10
workers. The first parallel version still ran only 2x faster end to end,
because every commit waited for two full device flushes (`F_FULLFSYNC` on
macOS, about 4 ms each). The drive serializes flushes, so adding threads did
not help. Group commit (see [Storage](storage.md)) brought 256 simulated scenes
from 2.10 s to 0.34 s on 8 workers, against 3.22 s serially.

For rendering at 640×480, 64 scenes took 1.26 s serially and 0.29 s with
8 physics and 8 renderer workers. Rendering workloads differ a lot by
resolution and camera count, so measure your own configuration.

# Server internals

`crates/bop-server/` · [code reference](../api/bop_server/index.html)

[Job server](../server.md) covers running and using the server. This chapter
covers how it is built.

## What it owns

An HTTP API over generation: an asset library, a job queue, job results in
object storage, export archives, and previews. It runs generation through
`bop-run` exactly like the command line, and adds the parts a shared service
needs: authentication, persistence, leases between servers, and uploads.

It does not render, simulate, or write dataset files itself. It must not
grow its own copy of anything `bop-run` or `bop-dataset` already does.

## Modules

| Module | Role |
|---|---|
| `lib.rs` | `build`: connect storage and the database, seed examples, rebuild the asset index, release expired jobs, build the router and the runner. |
| `main.rs` | Logging, settings, binding the port, graceful shutdown. |
| `settings.rs` | Every `BOP_*` environment variable, parsed once into `Settings`. |
| `api.rs` | Routes and their OpenAPI description, registered together with `utoipa`. Handlers, body limits, CORS, and file streaming. |
| `auth.rs` | `Authenticator`: the static API token, and OIDC access tokens checked locally against the provider's signing keys, which are cached. |
| `submission.rs` | Checking a submitted configuration and rewriting the parts the server controls. |
| `db.rs` | `JobStore`: every SQL statement, for SQLite and PostgreSQL. |
| `index.rs` | The storage index: listings and existence checks answered from the database. |
| `storage.rs` | `Storage`: an S3-compatible bucket or a local directory, through `object_store`, plus tar packing. |
| `runner.rs` | `Runner`: claims jobs, runs them, uploads results, finalizes, keeps leases and local data in order. |
| `previews.rs` | Scaled, coloured images of stored frames. |
| `examples.rs` | Seeding a new asset library with the example meshes. |

Migrations are in `crates/bop-server/migrations/`, plain SQL that runs on
both databases (TEXT and BIGINT only, times in Unix milliseconds).

## Where state lives

| State | Lives in | Source of truth |
|---|---|---|
| Job records, leases, cancel requests | `bop_jobs` | Database |
| Assets, job results, export archives | Storage, under `assets/`, `datasets/<job>/`, `exports/<job>/` | Storage |
| Indexes of the above | `bop_assets`, `bop_job_files`, `bop_exports` | Rebuilt from storage |
| A job in progress | `<BOP_DATA_DIR>/jobs/<job>/` | Rebuilt from storage when missing |
| Downloaded assets | `<BOP_DATA_DIR>/asset-cache/`, keyed by entity tag | Cache only |

The index is kept honest by ordering: a row is written after its object is
stored and deleted before its object is deleted. A crash can leave an object
without a row, never a row without an object, and listing storage repairs
it (`index.rs`).

## A job's life

```text
queued ──claim──► running ──► finalizing ──► succeeded
   ▲                │  │
   │                │  └──► failed
   └─lease expired──┘
                    └──cancel──► cancelled
```

- **Claiming** is a compare-and-set: `JobStore::claim_next` selects the
  oldest queued job and updates it with `WHERE state = 'queued'`. If another
  server got there first, the update changes no rows and it tries the next
  one. No lock is held across servers.
- **Leases** last 60 seconds and are renewed every 10 (`LEASE_MS`,
  `LEASE_RENEWAL` in `runner.rs`). Every server releases expired leases every
  15 seconds, so a job whose server died is queued again within about a
  minute. A renewal also reports cancel requests, so a cancel through any
  server reaches the one running the job.
- **Losing a lease** stops the job without recording anything: another
  server already owns it.
- **Continuing** a job elsewhere: `index::rebuild_local_copy` downloads its
  committed scenes and run files, and resume renders only what is missing.
- **Shutdown** cancels the run; in-flight scenes commit, and the job goes
  back to queued.

On PostgreSQL, every job change is also announced with `pg_notify` on the
`bop_jobs` channel. Event streams on every server follow it, and idle
runners wake at once instead of waiting for their 5-second poll.

## Changing it

- **API changes** must regenerate the committed contract:
  `BOP_BLESS=1 cargo test -p bop-server --test openapi`. The studio
  generates its client from `schemas/api.openapi.json`, so a change there is
  a change for the studio too.
- **New SQL** goes in `db.rs` and must run on both SQLite and PostgreSQL.
  Schema changes are new numbered migration files; never edit an applied
  one.
- **New stored files** need index rows written after the object and removed
  before it, and a way to rebuild them from a listing.
- **Tests**: `tests/api.rs` and `tests/leases.rs` run against SQLite and a
  local directory. `tests/end_to_end.rs` is `#[ignore]`d and runs a real job;
  CI runs it against PostgreSQL and MinIO.

## Caveats

- **No tenancy.** Authentication only decides who may call the API. There
  is one asset library and one job list for everyone, and jobs record no
  owner. Accounts, organizations, and projects need a new data model and an
  authorization check on every route.
- **One job per server.** A server runs one job at a time on its own
  renderer.
- **Handover limits.** The run manifest includes the absolute cache path and
  the render device, so servers sharing a database must use the same
  `BOP_DATA_DIR` path and the same kind of GPU and build to continue each
  other's jobs.
- **No request timings.** There is no per-request or per-stage timing log
  yet, so slow requests cannot be attributed to the server, storage, or the
  network from the logs alone.
- `api.rs` is about 1,300 lines; it is due to be split by resource (assets,
  jobs, exports).

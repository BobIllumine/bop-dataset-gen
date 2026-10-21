# Job server

`bop-server` runs generation jobs submitted over HTTP. It is a thin layer over
the same `bop-run` crate the `bop-gen generate` command uses, so a job produces
exactly the dataset the command line would.

- **Jobs** are recorded in SQLite or PostgreSQL.
- **Assets and results** live in an S3-compatible bucket (MinIO, S3) or a
  local directory.
- **One job runs at a time per server.** Each job already uses every
  configured simulation and render worker, and the GPU is the shared resource.

## Running it

| Setup | How |
| --- | --- |
| No services at all | `cargo run --release -p bop-server`. Uses SQLite and a directory below `./bop-data`, on `127.0.0.1:8080`. |
| Everything in containers | `docker compose -f deploy/compose.yaml --profile local up -d`. Starts PostgreSQL, MinIO, and a bucket-creation step. |
| External PostgreSQL / MinIO | Copy `deploy/.env.example` to `deploy/.env`, set the URLs and credentials, then `docker compose -f deploy/compose.yaml up -d`. |
| With the web UI | Add `--profile studio`. Builds bop-studio from `../bop-studio` and serves it on `127.0.0.1:3000`. |
| With authentik in containers | Add `-f deploy/authentik.yaml`; see [Authentication](#authentication). |

For a step-by-step production setup (GPU host, existing authentik, external
PostgreSQL and S3, Tailscale Funnel), see [Deploying](deployment.md).

The container needs credentials (`BOP_API_TOKEN` and/or `BOP_OIDC_*`) or an
explicit `BOP_ALLOW_ANONYMOUS=true`. Compose publishes every port on
`127.0.0.1` only; serve other machines through a TLS reverse proxy.

The container image (`deploy/Dockerfile`) targets Linux x86_64 with an NVIDIA
GPU and the NVIDIA Container Toolkit:

- wgpu renders through Vulkan. The NVIDIA runtime provides the Vulkan driver
  only when the `graphics` capability is requested, which the image and the
  compose file both do.
- BlenderProc and its pinned Blender 4.2.1 are installed when the image is
  built, not on the first job.

MinIO no longer publishes official container images. The `local` profile uses
the community fork `pgsty/minio`. Any S3-compatible store works in its place.

## Settings

| Variable | Default | Meaning |
| --- | --- | --- |
| `BOP_LISTEN` | `127.0.0.1:8080` (`0.0.0.0:8080` in the image) | Listen address. |
| `BOP_DATA_DIR` | `./bop-data` | Job working directories, the convex-decomposition and asset caches, and the default database and storage. |
| `BOP_DATABASE_URL` | `sqlite://<data>/bop.db?mode=rwc` | `postgres://user:password@host/db` or `sqlite://path`. Tables are created on start. |
| `BOP_STORAGE_URL` | `file://<data>/storage` | `s3://bucket[/prefix]` or `file:///directory`. The bucket must exist. |
| `AWS_ENDPOINT_URL`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_REGION` | | S3 connection. MinIO needs `AWS_ENDPOINT_URL`, and `AWS_ALLOW_HTTP=true` without TLS. |
| `BOP_STORAGE_CONCURRENCY` | `32` | Storage requests a job upload, asset download, or export keeps in flight. Lower it for a small MinIO. |
| `BOP_API_TOKEN` | unset | A shared token accepted as `Authorization: Bearer <token>`. |
| `BOP_OIDC_ISSUER`, `BOP_OIDC_CLIENT_ID` | unset | Accept access tokens from this OIDC provider, issued to this client. See [Authentication](#authentication). |
| `BOP_OIDC_GROUPS` | unset | Comma-separated; a token's `groups` claim must name one. |
| `BOP_OIDC_JWKS_URL` | from discovery | Where to fetch the provider's signing keys, when its public address is unreachable from the server. |
| `BOP_OIDC_SCOPES` | `openid profile email offline_access` | Scopes the web UI requests. |
| `BOP_ALLOW_ANONYMOUS` | `false` | Serve `/api` without credentials on a non-loopback address. Without it, such a server exits with status 2. |
| `BOP_MAX_SCENES` | `100000` | Largest `valid_scene_count` accepted. |
| `BOP_KEEP_LOCAL` | `false` | Keep a succeeded job's local copy after storing it. |
| `BOP_REQUEUE_ON_START` | `true` | At start, queue the jobs this server's previous process left running at once, instead of after their lease runs out. |
| `BOP_RUNNER_ID` | the host name | This server's name in job leases. Must be unique among servers sharing a database, and stable across restarts for `BOP_REQUEUE_ON_START` to recognize its own jobs. |
| `BOP_LOCAL_RETENTION_DAYS` | `14` | Delete local copies of failed and cancelled jobs untouched this long, and asset-cache entries of replaced or deleted assets. Resume rebuilds a deleted copy from storage. `0` keeps everything. |
| `BOP_BLENDERPROC_EXECUTABLE`, `BOP_BLENDERPROC_WORKER` | `blenderproc`, the bundled worker | The only BlenderProc programs a job can run. |
| `BOP_CORS_ORIGINS` | unset | Comma-separated browser origins, or `*`, allowed to call the API from another host, such as a separately hosted web UI. Unset allows same-origin only. |
| `BOP_LOG` | `info` | Log filter. |

The server checks storage and the database at start and exits with status 3
if either is unreachable.

## Authentication

With neither `BOP_API_TOKEN` nor `BOP_OIDC_*` set, `/api` is open, and the
server only starts that way on a loopback address or with
`BOP_ALLOW_ANONYMOUS=true`. Otherwise every `/api` route except
`GET /api/v1/auth` needs `Authorization: Bearer <token>`, where the token is
either:

- **the shared `BOP_API_TOKEN`**, for scripts and the CLI; or
- **an OIDC access token** from `BOP_OIDC_ISSUER`. The server checks it
  locally against the provider's published keys: an asymmetric signature
  (RS, PS, ES or EdDSA; never HMAC or `none`), `iss` equal to the issuer,
  `aud` containing `BOP_OIDC_CLIENT_ID`, `exp`, and, with `BOP_OIDC_GROUPS`,
  the `groups` claim. Keys are fetched at start and again, at most once a
  minute, when a token names an unknown key.

`GET /api/v1/auth` tells clients which applies, so the web UI can offer a
sign-in button. Failures answer 401, a user outside the allowed groups 403,
and an unreachable provider 503.

### authentik in containers

`deploy/authentik.yaml` adds authentik 2026.8 with its own PostgreSQL. A
blueprint (`deploy/authentik/blueprints/bop.yaml`) creates the public
`bop-studio` client, its application, and a `bop-users` group that is the only
one allowed to sign in, with `akadmin` in it.

```text
# deploy/.env
AUTHENTIK_SECRET_KEY=...         # openssl rand -base64 60
AUTHENTIK_PG_PASS=...
AUTHENTIK_BOOTSTRAP_PASSWORD=... # akadmin's password, set on first start
AUTHENTIK_BOOTSTRAP_EMAIL=admin@example.com
AUTHENTIK_URL=http://localhost:9000     # as browsers reach it
BOP_STUDIO_URL=http://localhost:3000    # as browsers reach it

docker compose -f deploy/compose.yaml -f deploy/authentik.yaml --profile local --profile studio up -d
```

The bootstrap password means authentik's initial-setup page is never open to
whoever reaches it first. The worker is not given the Docker socket, unlike
authentik's reference compose file. Add users to `bop-users` in authentik's
admin interface; self-enrollment is off by default.

### An authentik you already run

1. Create an *OAuth2/OpenID Provider*: client type **public**, a signing key
   (the self-signed certificate will do; without one authentik signs with
   HS256, which the server refuses), redirect URI
   `<studio origin>/auth/callback` (strict), grant types authorization code and
   refresh token, scopes `openid`, `profile`, `email`, `offline_access`.
2. Create an *Application* for it and bind the group allowed to use bop.
3. Set `BOP_OIDC_ISSUER` to the provider's *OpenID Configuration Issuer*
   (`https://auth.example.com/application/o/<slug>/`), `BOP_OIDC_CLIENT_ID`
   to its client ID, and `BOP_OIDC_GROUPS` to that group.

The studio calls authentik's token endpoint from the browser, so its
Content-Security-Policy must admit authentik's origin: any HTTPS origin by
default, or set `BOP_CONNECT_SRC` on the studio container.

### Hosting on a public address

- Terminate TLS in a reverse proxy and keep the published ports on
  `127.0.0.1`. Tokens are bearer credentials.
- Change the `local` profile's PostgreSQL and MinIO passwords, or use your own
  services; the defaults are public.
- API errors do not include internal details; the server log has them.
- Responses carry `X-Content-Type-Options: nosniff` and a sandboxing
  Content-Security-Policy, so uploaded files are never rendered as pages.
- Every authenticated user can see and change every job and asset. Allow only
  people you trust.

## Submitted configurations

A job is an ordinary run configuration, with these differences:

- **Asset paths:** each `source_path` is relative to the *asset library*: the
  files uploaded with `PUT /api/v1/assets/<path>`. Absolute paths, `..`, and
  hidden components are rejected, and every referenced asset must already be
  uploaded.
- **Cache path:** `[run] cache_path` is replaced with the server's shared cache.
- **BlenderProc paths:** `[renderers.blenderproc]` `executable_path` and
  `worker_path` are ignored. A configuration cannot choose which programs the
  server runs.
- **Output path:** `[run] output_path` is unused; results go to storage.

## Storage layout

```text
assets/<path>                    uploaded source files
datasets/<job-id>/...            the canonical dataset (as written by `bop-gen generate`)
exports/<job-id>/dataset.tar     the dataset, packed when the job finishes
exports/<job-id>/bop.tar         the BOP export, packed when the job finishes, if requested
```

Jobs from earlier versions may also hold `datasets/<job-id>/bop/...`, the
unpacked BOP export, and get their archives packed on first request.

- **When scenes are uploaded:** each scene is uploaded as soon as it commits,
  so partial results of a running, cancelled, or failed job are readable.
- **Run metadata:** `run.json`, `resolved-config.toml`, and
  `environment.json` go up with the first scenes, so another server can
  continue the run from storage. The models and any other files follow after
  the run.
- **Uploads:** up to `BOP_STORAGE_CONCURRENCY` files at a time; scenes that
  commit while an upload is in flight go up together next.
- **Finishing:** before a job succeeds, the dataset is validated as by
  `bop-gen validate`, then packed into its export archives from the local
  copy, so an export link is ready as soon as the job succeeds.
- **Resume:** scenes storage already holds are not uploaded again. Files
  written after the run are, since an attempt may change them.
- **Assets:** a job fetches its assets into `<data>/asset-cache`, keyed by
  the stored object's entity tag, and links them from there; a replaced
  asset is fetched again. Entries of replaced or deleted assets are removed
  after `BOP_LOCAL_RETENTION_DAYS`' hourly sweep.

### Storage index

The database keeps an index of storage: the asset library, every job's
stored files, and the export archives. Asset listings, the missing-asset
check, job file listings, and export links read it instead of storage.

- **Source of truth:** storage. Rows are written after the object and
  removed before it.
- **Rebuilding:** the asset index is rebuilt from a listing at every start,
  so assets changed in the bucket directly appear after a restart. A job
  stored by an earlier version is indexed from a listing on first use.
- **Concurrent exports:** a request for an archive another request is
  packing waits for it and shares the result. A claim that goes 30 s without
  progress, as after a crash, is taken over.
- **Local copy:** the copy under `<data>/jobs/<job-id>` is what resume
  continues from. It is deleted once a job succeeds, unless `BOP_KEEP_LOCAL`
  is set, and for failed and cancelled jobs after `BOP_LOCAL_RETENTION_DAYS`.
  When a job has no local copy, it is first rebuilt from storage: every scene
  whose files are all stored, and the run metadata. Scenes cut off mid-upload
  are rendered again.

| What | Where | Removed |
| --- | --- | --- |
| A job's working copy | `<data>/jobs/<job-id>` | When it succeeds (unless `BOP_KEEP_LOCAL`); failed or cancelled, after the retention period |
| Convex decompositions | `<data>/cache` | Never; delete it by hand to reclaim space |
| Downloaded assets | `<data>/asset-cache` | Versions no longer in the library, after the retention period |

## Lifecycle

`queued → running → finalizing → succeeded`, or `failed` / `cancelled`.

- **Cancel:** cancelling a queued job takes effect at once. A running job
  stops after its in-flight scenes commit, whichever server it runs on.
- **Resume:** `failed` and `cancelled` jobs can be resumed. The run continues
  from the scenes already committed, on any server.
- **Server shutdown:** SIGTERM or Ctrl-C lets in-flight scenes commit, then
  returns the job to `queued`. The next start, or another server, continues
  it. This is why the compose file sets a long `stop_grace_period`.

### Several servers

Servers sharing a database (and storage) share the queue.

- **Leases:** the server running a job renews a 60-second lease every
  10 seconds. Every server checks every 15 seconds for jobs whose lease ran
  out, such as after a crash, and queues them again (or marks them
  `cancelled` if a cancel was pending). A server that finds it lost a lease
  stops the job without recording anything.
- **Continuing elsewhere:** the server that picks up such a job rebuilds the
  local copy from storage, so committed scenes are not rendered again. That
  needs the same `BOP_DATA_DIR` path (the resolved configuration records the
  cache inside it; containers all use `/var/lib/bop`) and the same render
  device and build, since a dataset never mixes outputs of different
  platforms. Otherwise the job fails with "existing dataset manifest is
  incompatible", and resuming it on a matching server continues it.
- **Live updates:** on PostgreSQL, every job change is announced with
  `NOTIFY`, so event streams on every server follow jobs running on any of
  them, and an idle server starts a new job at once. On SQLite, a single
  server, idle servers check every 5 seconds.

## API

All routes are below `/api/v1` except `GET /healthz`. Errors are
`{"error": "..."}`. The full contract is `schemas/api.openapi.json`
(OpenAPI 3.1), generated from the router; clients such as the web UI generate
their types from it.

| Route | |
| --- | --- |
| `GET /auth` | `{required, api_token, oidc: {issuer, client_id, scopes}}`. Needs no credentials. |
| `GET /version` | `{server, api, renderers, max_scenes, signed_exports}`: what this server is and can do. |
| `GET /assets` | `[{path, size}]` |
| `GET /assets/<path>` | Streams one asset file. |
| `PUT /assets/<path>` | Raw body, at most 64 MiB. |
| `DELETE /assets/<path>` | |
| `POST /asset-optimizations` | `{"path", "max_deviation_mm": 0.1, "crease_angle_deg": 30, "source_unit_scale_m"}`. Writes `<name>.optimized.glb` next to an STL or GLB, replacing an earlier one, and returns 201 with `{path, source_bytes, bytes, source_triangles, triangles, source_vertices, vertices, max_deviation_mm}`. `source_unit_scale_m` is required for STL. See [`optimize-asset`](cli.md#optimizing-a-mesh). 422 when the mesh cannot be optimized within the budget. |
| `POST /configs/validate` | `{"config": "<toml>"}`. Checks it exactly as job submission would and returns `{total_scenes, asset_paths, missing_assets}`, or 422. Queues nothing. |
| `POST /jobs` | `{"config": "<toml>", "renderer": "wgpu" \| "blenderproc", "export_bop": bool}`. `export_bop` defaults to true: the finished dataset is also converted to [BOP](bop.md). Returns 201 with the job, 422 for a rejected configuration. |
| `GET /jobs?limit=N` | Newest first. |
| `GET /jobs/<id>` | `{id, state, renderer, export_bop, total_scenes, committed_scenes, error, created_at_ms, updated_at_ms, started_at_ms, finished_at_ms, cancel_requested}`. `cancel_requested` is true while a running job finishes its in-flight scenes after a cancel. |
| `GET /jobs/<id>/config` | The submitted TOML. |
| `POST /jobs/<id>/cancel` | 204 if it was queued, 202 if it was running here. |
| `POST /jobs/<id>/resume` | 202, or 409 unless the job is failed or cancelled. |
| `GET /jobs/<id>/events` | Server-sent `job` events: the current state, then every state and progress change. |
| `GET /jobs/<id>/files` | Stored files below `datasets/<id>/`. |
| `GET /jobs/<id>/files/<path>` | Streams one stored file. |
| `POST /jobs/<id>/exports` | `{"format": "dataset" \| "bop", "expires_in_s": 60..604800}`. Returns `{format, url, signed, expires_at_ms, size, file_name}` for a succeeded job's uncompressed tar, packing it first for jobs from earlier versions. |
| `GET /jobs/<id>/exports/<format>.tar` | Streams a packed export, for storage that cannot sign URLs. |

- **Export links:** with S3 storage, `url` is a presigned URL that needs no
  credentials until `expires_at_ms`. Its host is the storage endpoint, so set
  `AWS_ENDPOINT_URL` to an address the downloading browser can reach. With
  local storage, `url` is the API route above and needs the bearer token.
- **Packing:** the first export request for a format streams every file into
  the archive before it returns; later requests only sign a new link.

```text
curl -X PUT --data-binary @cup.glb localhost:8080/api/v1/assets/cup/cup.glb
jq -n --rawfile config cup_nippel.toml '{config: $config, renderer: "wgpu"}' \
  | curl -H 'content-type: application/json' -d @- localhost:8080/api/v1/jobs
curl -N localhost:8080/api/v1/jobs/<id>/events
```

## Limits

- **Several servers on one database:** see [Several servers](#several-servers)
  for what a job needs to continue on another server.
- **Authorization:** authentication is all or nothing. There are no roles or
  per-user jobs; anyone the server accepts can use every route.
- **Depth previews:** depth is stored as EXR, which browsers do not display.
  A UI needs a server-side preview conversion.

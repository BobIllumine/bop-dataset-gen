# Deploying

This walks through one deployment end to end: bop-server and the studio on a
GPU machine, sign-in through an authentik you already run, jobs in your own
PostgreSQL, datasets in your own S3-compatible bucket, and the studio
published with Tailscale Funnel.

```text
browser ──https──▶ Tailscale Funnel ──▶ 127.0.0.1:3000  studio (Caddy)
   │                                        └── /api ──▶ bop-server :8080 (container network only)
   │                                                        ├──▶ PostgreSQL
   │                                                        ├──▶ S3 bucket
   └──https──▶ authentik (org network) ◀── signing keys ────┘
```

Only the studio is published. Its Caddy serves the app and proxies `/api` to
bop-server, which is not reachable from outside the machine. From the public
internet, everything but the static app and two discovery routes (`/healthz`,
`/api/v1/auth`) answers 401 without an authentik token, and only people who can
reach your authentik can get one.

## 1. Prerequisites

On the deploy machine:

- **Linux x86_64 with an NVIDIA GPU** and its driver. The image renders with
  wgpu (Vulkan) and BlenderProc (Cycles).
- **Docker Engine with Compose v2.20 or later**, and the
  [NVIDIA Container Toolkit](https://docs.nvidia.com/datacenter/cloud-native/container-toolkit/latest/install-guide.html)
  configured for Docker (`sudo nvidia-ctk runtime configure --runtime=docker`,
  then restart Docker). Check with
  `docker run --rm --gpus all ubuntu nvidia-smi`.
- **Tailscale**, logged in to your tailnet, with Funnel allowed for this
  machine in the tailnet policy (the `funnel` node attribute) and HTTPS
  certificates enabled.
- **Network reach** from this machine to authentik, PostgreSQL and the bucket.

Note the machine's Funnel name, `https://<machine>.<tailnet>.ts.net`. It is
the studio's address below.

## 2. Get the deployment files

Released images are published to GitHub Packages, so the machine only needs
this repository's `deploy/` directory at the release:

```text
git clone --branch v0.1.3 https://github.com/BobIllumine/bop-dataset-gen.git
```

| Image | |
| --- | --- |
| `ghcr.io/bobillumine/bop-server:0.1.3` | linux/amd64, NVIDIA |
| `ghcr.io/bobillumine/bop-studio:0.1.3` | linux/amd64 and linux/arm64 |

To build from source instead, also clone bop-studio next to it
(`git clone --branch v0.1.3 https://github.com/BobIllumine/bop-studio.git`),
leave the two `*_IMAGE` variables below unset, and add `--build` in step 6.

## 3. authentik

authentik must be served over **HTTPS**: the studio page is HTTPS, and
browsers refuse its calls to a plain-HTTP token endpoint.

In the authentik admin interface:

1. **Applications → Providers → Create → OAuth2/OpenID Provider**
   - Name: `bop studio`
   - Authorization flow: `default-provider-authorization-implicit-consent`
   - Client type: **Public**
   - Client ID: note it (or set it to `bop-studio`)
   - Redirect URI: **Strict**, `https://<machine>.<tailnet>.ts.net/auth/callback`
   - Signing key: **`authentik Self-signed Certificate`** (or any RSA/EC
     key). Without a signing key, authentik signs tokens with HS256, which
     bop-server rejects.
   - Advanced: grant types *Authorization Code* and *Refresh Token*; scopes
     `openid`, `email`, `profile`, `offline_access`; access token validity
     `minutes=10`.
2. **Applications → Create**: name `bop studio`, slug `bop`, provider
   `bop studio`.
3. **Directory → Groups → Create** `bop-users`, add the people who may use
   bop, then on the application's **Policy / Group / User Bindings** tab bind
   that group. With a binding, nobody else can sign in to the application.
4. Copy the provider's **OpenID Configuration Issuer**, e.g.
   `https://auth.example.org/application/o/bop/`, trailing slash included.

From the deploy machine, check that the keys are reachable:

```text
curl -fsS https://auth.example.org/application/o/bop/jwks/
```

If authentik's public name does not resolve there but an internal one does,
set `BOP_OIDC_JWKS_URL` to the internal JWKS URL in step 5.

## 4. PostgreSQL and the bucket

- **PostgreSQL:** an empty database and a user that owns it. Tables are
  created on first start.

  ```sql
  CREATE USER bop WITH PASSWORD '...';
  CREATE DATABASE bop OWNER bop;
  ```

- **Bucket:** create it (the server does not), and an access key limited to
  it. Results go below an optional prefix. On first start the server adds the
  example meshes the studio's presets use; see
  [Example assets](server.md#example-assets).

Export links are presigned URLs on the bucket's endpoint, so a browser
downloads an export only where it can reach `AWS_ENDPOINT_URL`.

## 5. Configure

Create `bop-dataset-gen/deploy/.env`:

```text
# PostgreSQL. Add ?sslmode=require for TLS.
BOP_DATABASE_URL=postgres://bop:PASSWORD@db.example.org:5432/bop

# Bucket and optional prefix; AWS_ENDPOINT_URL for anything but AWS itself.
BOP_STORAGE_URL=s3://bop-datasets/production
AWS_ENDPOINT_URL=https://s3.example.org
AWS_REGION=us-east-1
AWS_ACCESS_KEY_ID=...
AWS_SECRET_ACCESS_KEY=...
# Only for an endpoint without TLS:
#AWS_ALLOW_HTTP=true

# authentik
BOP_OIDC_ISSUER=https://auth.example.org/application/o/bop/
BOP_OIDC_CLIENT_ID=bop-studio
BOP_OIDC_GROUPS=bop-users
#BOP_OIDC_JWKS_URL=https://authentik.internal/application/o/bop/jwks/

# Leave unset unless scripts need it; if set, use `openssl rand -hex 32`.
#BOP_API_TOKEN=

# The released images.
BOP_SERVER_IMAGE=ghcr.io/bobillumine/bop-server:0.1.3
BOP_STUDIO_IMAGE=ghcr.io/bobillumine/bop-studio:0.1.3
```

Leave `BOP_ALLOW_ANONYMOUS`, `BOP_CORS_ORIGINS` and the `*_BIND` variables
unset: the defaults keep both ports on `127.0.0.1` and require sign-in.

The studio's Content-Security-Policy allows calls to any HTTPS origin by
default, which covers authentik. To pin it, set
`BOP_CONNECT_SRC=https://auth.example.org`.

## 6. Build and start

```text
cd bop-dataset-gen
docker compose -f deploy/compose.yaml --profile studio pull
docker compose -f deploy/compose.yaml --profile studio up -d
```

The server image is large (it includes BlenderProc and Blender), so the first
pull takes a while. Building from source with `up -d --build` instead takes
20 minutes or more.

Check it:

```text
docker compose -f deploy/compose.yaml logs bop-server | grep -E "accepting OIDC|serving|ERROR|WARN"
curl -fsS http://127.0.0.1:8080/healthz                  # ok
curl -fsS http://127.0.0.1:8080/api/v1/auth              # names your issuer
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:8080/api/v1/jobs   # 401
docker compose -f deploy/compose.yaml exec bop-server /opt/bop/bop-gen doctor --renderer wgpu
curl -fsS http://127.0.0.1:3000/ | head -c 100          # the studio
```

The log line `accepting OIDC access tokens` means the server fetched
authentik's keys. A warning instead means it could not reach them yet. It
retries on the first sign-in, but fix the address (or `BOP_OIDC_JWKS_URL`)
before going further.

## 7. Publish with Funnel

```text
sudo tailscale funnel --bg 3000
tailscale funnel status
```

This publishes `https://<machine>.<tailnet>.ts.net` → `127.0.0.1:3000` and
keeps it across reboots. Publish only the studio's port. Never funnel `8080`
(bop-server) or anything else from this stack.

## 8. Sign in

From a device on your org network, open
`https://<machine>.<tailnet>.ts.net`, select **Sign in**, and log in to
authentik. The top bar then shows your username. Members of `bop-users` can
use everything; anyone else sees *Your account is not allowed to use this
server*.

## Operating it

- **Upgrading:** check out the new tag, set both `*_IMAGE` variables to the
  new version, then `docker compose -f deploy/compose.yaml --profile studio pull`
  and `up -d`.
  Stopping lets in-flight scenes commit (up to the 5-minute grace period), and
  the interrupted job continues on the next start.
- **Backups:** PostgreSQL and the bucket hold everything that matters. The
  `bop-data` volume holds caches and the local copy of unfinished jobs, which
  resume rebuilds from the bucket when it is gone.
- **Logs:** `docker compose -f deploy/compose.yaml logs -f bop-server`.
  `BOP_LOG=debug` also logs why each rejected request failed.
- **Determinism:** datasets are byte-identical only on the same platform (see
  [Determinism](determinism.md)). Resume a job on the machine type that
  started it.

## When something is wrong

| Symptom | Cause |
| --- | --- |
| Server exits with status 2 | A setting is malformed or credentials are missing; the log names it. |
| Server exits with status 3 | PostgreSQL or the bucket is unreachable or rejects the credentials. |
| Studio banner: *the sign-in provider is unreachable* (HTTP 503) | bop-server cannot fetch authentik's keys. Set `BOP_OIDC_JWKS_URL`. |
| 401 right after signing in | The token's issuer or audience does not match: `BOP_OIDC_ISSUER` must equal the provider's issuer exactly (trailing slash), and `BOP_OIDC_CLIENT_ID` its client ID. `BOP_LOG=debug` shows which. |
| authentik shows *redirect URI error* | The provider's redirect URI is not exactly `https://<machine>.<tailnet>.ts.net/auth/callback`. |
| Sign-in loops or fails silently | The browser console shows a Content-Security-Policy or mixed-content error: authentik must be HTTPS, and allowed by `BOP_CONNECT_SRC`. |
| Jobs fail at once with a renderer error | The GPU is not visible in the container; run `bop-gen doctor` as in step 6 and check the NVIDIA Container Toolkit. |

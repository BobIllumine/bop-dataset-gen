//! Server settings, read from `BOP_*` environment variables.

use std::{net::SocketAddr, path::PathBuf};

use bop_run::BlenderProcRuntime;

/// Everything the server needs to start.
#[derive(Clone, Debug)]
pub struct Settings {
    /// `BOP_LISTEN`: address to serve on. Default `127.0.0.1:8080`; the
    /// container image sets `0.0.0.0:8080`.
    pub listen: SocketAddr,
    /// `BOP_DATA_DIR`: local working directory for jobs, the decomposition
    /// cache, and the default database and storage. Default `./bop-data`.
    pub data_dir: PathBuf,
    /// `BOP_DATABASE_URL`: `postgres://...` or `sqlite://...`.
    /// Default `sqlite://<data_dir>/bop.db?mode=rwc`.
    pub database_url: String,
    /// `BOP_STORAGE_URL`: `s3://bucket[/prefix]` or `file:///directory`.
    /// Default `file://<data_dir>/storage`.
    pub storage_url: String,
    /// `BOP_API_TOKEN`: when set, `/api` requests may authenticate with
    /// `Authorization: Bearer <token>`.
    pub api_token: Option<String>,
    /// `BOP_OIDC_*`: when set, `/api` requests may authenticate with an access
    /// token from this OIDC provider, such as authentik.
    pub oidc: Option<OidcSettings>,
    /// `BOP_ALLOW_ANONYMOUS`: serve `/api` without authentication on a
    /// non-loopback address. Default false: such a server refuses to start.
    pub allow_anonymous: bool,
    /// `BOP_MAX_SCENES`: largest `valid_scene_count` a job may request. Default 100000.
    pub max_scenes: u32,
    /// `BOP_KEEP_LOCAL`: keep a succeeded job's local copy after storing it. Default false.
    pub keep_local: bool,
    /// `BOP_REQUEUE_ON_START`: queue jobs a previous process left running.
    /// Default true; turn off when several servers share one database.
    pub requeue_on_start: bool,
    /// `BOP_BLENDERPROC_EXECUTABLE` and `BOP_BLENDERPROC_WORKER`. Submitted
    /// configurations cannot choose these.
    pub blenderproc: BlenderProcRuntime,
    /// `BOP_CORS_ORIGINS`: comma-separated browser origins allowed to call the
    /// API from another host, or `*`. Default none (same origin only).
    pub cors_origins: Vec<String>,
}

impl Settings {
    /// Reads settings from the process environment.
    ///
    /// # Errors
    ///
    /// Returns a message naming the malformed variable.
    pub fn from_env() -> Result<Self, String> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// Reads settings through `var`, which returns a variable's value if set.
    ///
    /// # Errors
    ///
    /// Returns a message naming the malformed variable.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let var = |name: &str| var(name).filter(|value| !value.is_empty());
        let listen = var("BOP_LISTEN")
            .unwrap_or_else(|| "127.0.0.1:8080".to_owned())
            .parse()
            .map_err(|error| format!("BOP_LISTEN: {error}"))?;
        let data_dir = PathBuf::from(var("BOP_DATA_DIR").unwrap_or_else(|| "bop-data".to_owned()));
        let data_dir = std::path::absolute(&data_dir)
            .map_err(|error| format!("BOP_DATA_DIR {}: {error}", data_dir.display()))?;
        let database_url = var("BOP_DATABASE_URL")
            .unwrap_or_else(|| format!("sqlite://{}?mode=rwc", data_dir.join("bop.db").display()));
        let storage_url = var("BOP_STORAGE_URL")
            .unwrap_or_else(|| format!("file://{}", data_dir.join("storage").display()));
        let max_scenes = var("BOP_MAX_SCENES")
            .map_or(Ok(100_000), |value| value.parse())
            .map_err(|error| format!("BOP_MAX_SCENES: {error}"))?;
        let flag = |name: &str, default: bool| match var(name).as_deref() {
            None => Ok(default),
            Some("1" | "true") => Ok(true),
            Some("0" | "false") => Ok(false),
            Some(other) => Err(format!("{name} must be true or false, not {other:?}")),
        };
        let api_token = var("BOP_API_TOKEN");
        let oidc = OidcSettings::from_vars(&var)?;
        let allow_anonymous = flag("BOP_ALLOW_ANONYMOUS", false)?;
        if api_token.is_none() && oidc.is_none() && !allow_anonymous && !is_loopback(&listen) {
            return Err(format!(
                "refusing to serve {listen} without authentication: set BOP_API_TOKEN or \
                 BOP_OIDC_ISSUER and BOP_OIDC_CLIENT_ID, or BOP_ALLOW_ANONYMOUS=true"
            ));
        }
        Ok(Self {
            listen,
            database_url,
            storage_url,
            api_token,
            oidc,
            allow_anonymous,
            max_scenes,
            keep_local: flag("BOP_KEEP_LOCAL", false)?,
            requeue_on_start: flag("BOP_REQUEUE_ON_START", true)?,
            blenderproc: BlenderProcRuntime {
                executable: Some(
                    var("BOP_BLENDERPROC_EXECUTABLE")
                        .map_or_else(|| PathBuf::from("blenderproc"), PathBuf::from),
                ),
                worker: Some(
                    var("BOP_BLENDERPROC_WORKER").map_or_else(default_worker_path, PathBuf::from),
                ),
            },
            cors_origins: list(var("BOP_CORS_ORIGINS")),
            data_dir,
        })
    }

    /// Directory holding one working directory per job.
    #[must_use]
    pub fn jobs_dir(&self) -> PathBuf {
        self.data_dir.join("jobs")
    }

    /// Shared convex-decomposition cache, written into every job's configuration.
    #[must_use]
    pub fn cache_dir(&self) -> PathBuf {
        self.data_dir.join("cache")
    }
}

/// An OIDC provider whose access tokens the API accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OidcSettings {
    /// `BOP_OIDC_ISSUER`: the exact `iss` of accepted tokens, such as
    /// `https://auth.example.com/application/o/bop/` for authentik. Discovery
    /// is read from `<issuer>/.well-known/openid-configuration`.
    pub issuer: String,
    /// `BOP_OIDC_CLIENT_ID`: the client the web UI signs in as; accepted
    /// tokens must name it in `aud`.
    pub client_id: String,
    /// `BOP_OIDC_JWKS_URL`: where to fetch signing keys instead of discovery,
    /// for when the issuer's public address is unreachable from the server.
    pub jwks_url: Option<String>,
    /// `BOP_OIDC_GROUPS`: comma-separated groups; when set, a token's `groups`
    /// claim must name at least one.
    pub groups: Vec<String>,
    /// `BOP_OIDC_SCOPES`: scopes the web UI requests.
    /// Default `openid profile email offline_access`.
    pub scopes: String,
}

impl OidcSettings {
    fn from_vars(var: &impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        let (issuer, client_id) = match (var("BOP_OIDC_ISSUER"), var("BOP_OIDC_CLIENT_ID")) {
            (None, None) => return Ok(None),
            (Some(issuer), Some(client_id)) => (issuer, client_id),
            _ => {
                return Err(
                    "BOP_OIDC_ISSUER and BOP_OIDC_CLIENT_ID must be set together".to_owned(),
                );
            }
        };
        for (name, url) in [
            ("BOP_OIDC_ISSUER", Some(&issuer)),
            ("BOP_OIDC_JWKS_URL", var("BOP_OIDC_JWKS_URL").as_ref()),
        ] {
            if let Some(url) = url
                && !(url.starts_with("https://") || url.starts_with("http://"))
            {
                return Err(format!(
                    "{name} must be an http:// or https:// URL, not {url:?}"
                ));
            }
        }
        Ok(Some(Self {
            issuer,
            client_id,
            jwks_url: var("BOP_OIDC_JWKS_URL"),
            groups: list(var("BOP_OIDC_GROUPS")),
            scopes: var("BOP_OIDC_SCOPES")
                .unwrap_or_else(|| "openid profile email offline_access".to_owned()),
        }))
    }
}

fn list(value: Option<String>) -> Vec<String> {
    value
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn is_loopback(listen: &SocketAddr) -> bool {
    listen.ip().is_loopback()
}

/// `blenderproc-worker/` beside the executable (the container layout), else this checkout.
fn default_worker_path() -> PathBuf {
    let bundled = std::env::current_exe().ok().and_then(|executable| {
        executable
            .parent()
            .map(|directory| directory.join("blenderproc-worker/blenderproc_worker.py"))
    });
    match bundled {
        Some(path) if path.is_file() => path,
        _ => std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tools/blenderproc-worker/blenderproc_worker.py"),
    }
}

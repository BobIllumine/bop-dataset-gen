//! Request authentication: the shared `BOP_API_TOKEN`, and access tokens from
//! an OIDC provider such as authentik.
//!
//! Provider tokens are JWTs checked locally against the provider's published
//! signing keys: signature, `iss`, `aud`, `exp`, and optionally `groups`.
//! Only asymmetric algorithms are accepted.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use jsonwebtoken::{
    Algorithm, DecodingKey, Validation,
    jwk::{AlgorithmParameters, Jwk, JwkSet},
};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::RwLock;

use crate::settings::{OidcSettings, Settings};

/// Algorithms a provider token may be signed with. HMAC and `none` are never
/// accepted: a public client's tokens must not be verifiable with a shared secret.
const ALGORITHMS: [Algorithm; 9] = [
    Algorithm::RS256,
    Algorithm::RS384,
    Algorithm::RS512,
    Algorithm::PS256,
    Algorithm::PS384,
    Algorithm::PS512,
    Algorithm::ES256,
    Algorithm::ES384,
    Algorithm::EdDSA,
];
/// Shortest interval between key fetches, so unknown key ids cannot make the
/// server hammer the provider.
const REFRESH_INTERVAL: Duration = Duration::from_mins(1);
/// Longest wait for the provider's discovery document or keys.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Why a request was not authenticated.
#[derive(Debug, Error)]
pub enum AuthError {
    /// No bearer token, or a static token that does not match.
    #[error("missing or wrong bearer token")]
    Missing,
    /// The token is not an acceptable provider token.
    #[error("invalid token: {0}")]
    Invalid(String),
    /// The token is valid but its user is in none of the required groups.
    #[error("user is not in an allowed group")]
    Forbidden,
    /// The provider's keys could not be fetched.
    #[error("cannot fetch signing keys: {0}")]
    Keys(String),
}

/// Who made a request.
#[derive(Clone, Debug)]
pub enum Identity {
    /// The server has no authentication configured.
    Anonymous,
    /// The shared `BOP_API_TOKEN`.
    ApiToken,
    /// A provider user.
    User {
        /// The token's `sub`.
        subject: String,
        /// `preferred_username`, when the token carries it.
        username: Option<String>,
    },
}

/// Checks requests against the configured credentials.
#[derive(Clone)]
pub struct Authenticator {
    api_token: Option<String>,
    oidc: Option<Arc<Oidc>>,
}

impl Authenticator {
    /// Builds the checks `settings` asks for.
    ///
    /// # Errors
    ///
    /// Returns a message if the HTTP client cannot be built.
    pub fn new(settings: &Settings) -> Result<Self, String> {
        let oidc = match &settings.oidc {
            Some(oidc) => Some(Arc::new(Oidc::new(oidc.clone())?)),
            None => None,
        };
        Ok(Self {
            api_token: settings.api_token.clone(),
            oidc,
        })
    }

    /// Whether requests need credentials at all.
    #[must_use]
    pub const fn required(&self) -> bool {
        self.api_token.is_some() || self.oidc.is_some()
    }

    /// Fetches the provider's keys now, so a misconfiguration shows at start.
    ///
    /// # Errors
    ///
    /// Returns why the keys could not be fetched.
    pub async fn prefetch(&self) -> Result<(), AuthError> {
        match &self.oidc {
            Some(oidc) => oidc.refresh(true).await,
            None => Ok(()),
        }
    }

    /// Authenticates the bearer token of a request, if any.
    ///
    /// # Errors
    ///
    /// Returns why the request is not authenticated.
    pub async fn authenticate(&self, bearer: Option<&str>) -> Result<Identity, AuthError> {
        if !self.required() {
            return Ok(Identity::Anonymous);
        }
        let token = bearer.ok_or(AuthError::Missing)?;
        if let Some(expected) = &self.api_token
            && constant_time_eq(token.as_bytes(), expected.as_bytes())
        {
            return Ok(Identity::ApiToken);
        }
        match &self.oidc {
            // Anything that is not even shaped like a JWT was a wrong static token.
            Some(oidc) if token.matches('.').count() == 2 => oidc.verify(token).await,
            _ => Err(AuthError::Missing),
        }
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

/// Claims read from a provider token; `iss`, `aud` and `exp` are checked by
/// the validation itself.
#[derive(Deserialize)]
struct Claims {
    sub: String,
    preferred_username: Option<String>,
    #[serde(default)]
    groups: Vec<String>,
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
}

struct Keys {
    set: JwkSet,
    fetched: Option<Instant>,
}

struct Oidc {
    settings: OidcSettings,
    http: reqwest::Client,
    keys: RwLock<Keys>,
}

impl Oidc {
    fn new(settings: OidcSettings) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .map_err(|error| format!("OIDC HTTP client: {error}"))?;
        Ok(Self {
            settings,
            http,
            keys: RwLock::new(Keys {
                set: JwkSet { keys: Vec::new() },
                fetched: None,
            }),
        })
    }

    async fn verify(&self, token: &str) -> Result<Identity, AuthError> {
        let header = jsonwebtoken::decode_header(token)
            .map_err(|error| AuthError::Invalid(error.to_string()))?;
        if !ALGORITHMS.contains(&header.alg) {
            return Err(AuthError::Invalid(format!(
                "algorithm {:?} is not accepted",
                header.alg
            )));
        }
        let kid = header.kid.as_deref();
        let jwk = if let Some(jwk) = self.find(kid).await {
            jwk
        } else {
            // The provider may have rotated its keys.
            self.refresh(false).await?;
            self.find(kid)
                .await
                .ok_or_else(|| AuthError::Invalid("unknown signing key".to_owned()))?
        };
        let key = decoding_key(&jwk, header.alg)?;
        let mut validation = Validation::new(header.alg);
        validation.set_issuer(&[&self.settings.issuer]);
        validation.set_audience(&[&self.settings.client_id]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        let claims = jsonwebtoken::decode::<Claims>(token, &key, &validation)
            .map_err(|error| AuthError::Invalid(error.to_string()))?
            .claims;
        if !self.settings.groups.is_empty()
            && !claims
                .groups
                .iter()
                .any(|group| self.settings.groups.contains(group))
        {
            return Err(AuthError::Forbidden);
        }
        Ok(Identity::User {
            subject: claims.sub,
            username: claims.preferred_username,
        })
    }

    /// The key named `kid`, or the only key when the token names none.
    async fn find(&self, kid: Option<&str>) -> Option<Jwk> {
        let keys = self.keys.read().await;
        match kid {
            Some(kid) => keys.set.find(kid).cloned(),
            None if keys.set.keys.len() == 1 => keys.set.keys.first().cloned(),
            None => None,
        }
    }

    /// Fetches the key set, at most once per [`REFRESH_INTERVAL`] unless `force`.
    async fn refresh(&self, force: bool) -> Result<(), AuthError> {
        let mut keys = self.keys.write().await;
        if !force
            && keys
                .fetched
                .is_some_and(|at| at.elapsed() < REFRESH_INTERVAL)
        {
            return Ok(());
        }
        // Counts as an attempt even if it fails, so failures are rate-limited too.
        keys.fetched = Some(Instant::now());
        keys.set = self.fetch_keys().await?;
        tracing::info!(keys = keys.set.keys.len(), "fetched OIDC signing keys");
        Ok(())
    }

    async fn fetch_keys(&self) -> Result<JwkSet, AuthError> {
        let jwks_url = if let Some(url) = &self.settings.jwks_url {
            url.clone()
        } else {
            let url = format!(
                "{}/.well-known/openid-configuration",
                self.settings.issuer.trim_end_matches('/')
            );
            let discovery: Discovery = self.get_json(&url).await?;
            if discovery.issuer != self.settings.issuer {
                return Err(AuthError::Keys(format!(
                    "{url} names issuer {:?}, but BOP_OIDC_ISSUER is {:?}",
                    discovery.issuer, self.settings.issuer
                )));
            }
            discovery.jwks_uri
        };
        self.get_json(&jwks_url).await
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, AuthError> {
        let failed = |error: reqwest::Error| AuthError::Keys(format!("{url}: {error}"));
        self.http
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(failed)?
            .json()
            .await
            .map_err(failed)
    }
}

/// The verification key for `jwk`, if it may verify `algorithm`.
fn decoding_key(jwk: &Jwk, algorithm: Algorithm) -> Result<DecodingKey, AuthError> {
    let invalid = |message: &str| AuthError::Invalid(message.to_owned());
    if matches!(jwk.algorithm, AlgorithmParameters::OctetKey(_)) {
        return Err(invalid("symmetric signing keys are not accepted"));
    }
    if let Some(declared) = jwk.common.key_algorithm
        && format!("{declared:?}") != format!("{algorithm:?}")
    {
        return Err(invalid("token algorithm does not match its signing key"));
    }
    DecodingKey::from_jwk(jwk).map_err(|error| AuthError::Invalid(error.to_string()))
}

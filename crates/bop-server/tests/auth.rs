//! Authentication: the shared token, OIDC access tokens checked against a
//! local stand-in for the provider, and the refusal to serve anonymously.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
    routing::get,
};
use jsonwebtoken::{Algorithm, EncodingKey, Header, jwk::Jwk};
use serde_json::{Value, json};
use tower::ServiceExt as _;

/// A throwaway key generated for these tests only.
const KEY_PEM: &[u8] = include_bytes!("fixtures/oidc-test-key.pem");
const CLIENT_ID: &str = "bop-studio";
const KID: &str = "test-key";

/// A provider serving discovery and its key set on a local port.
struct Provider {
    issuer: String,
    key: EncodingKey,
}

impl Provider {
    async fn start() -> Self {
        let key = EncodingKey::from_rsa_pem(KEY_PEM).unwrap();
        let mut jwk = Jwk::from_encoding_key(&key, Algorithm::RS256).unwrap();
        jwk.common.key_id = Some(KID.to_owned());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!(
            "http://{}/application/o/bop/",
            listener.local_addr().unwrap()
        );
        let discovery = json!({
            "issuer": issuer,
            "jwks_uri": format!("{issuer}jwks/"),
        });
        let keys = json!({ "keys": [jwk] });
        let app = Router::new()
            .route(
                "/application/o/bop/.well-known/openid-configuration",
                get(move || async move { Json(discovery) }),
            )
            .route(
                "/application/o/bop/jwks/",
                get(move || async move { Json(keys) }),
            );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { issuer, key }
    }

    /// A token as the provider would issue it, with `patch` merged into its claims.
    fn token(&self, patch: &Value) -> String {
        let mut claims = json!({
            "iss": self.issuer,
            "aud": CLIENT_ID,
            "sub": "user-1",
            "preferred_username": "alice",
            "groups": ["bop-users"],
            "exp": now() + 300,
            "iat": now(),
        });
        for (name, value) in patch.as_object().unwrap() {
            claims[name] = value.clone();
        }
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(KID.to_owned());
        jsonwebtoken::encode(&header, &claims, &self.key).unwrap()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

async fn app(root: &std::path::Path, vars: &[(&str, &str)]) -> Router {
    let mut vars: Vec<(String, String)> = vars
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    vars.push(("BOP_DATA_DIR".to_owned(), root.display().to_string()));
    let settings = bop_server::settings::Settings::from_vars(move |name| {
        vars.iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    })
    .unwrap();
    bop_server::build(settings).await.unwrap().0
}

async fn get_status(app: &Router, uri: &str, bearer: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::builder().uri(uri);
    if let Some(token) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn provider_tokens_are_checked_for_signature_issuer_audience_and_expiry() {
    let provider = Provider::start().await;
    let root = tempfile::tempdir().unwrap();
    let app = app(
        root.path(),
        &[
            ("BOP_OIDC_ISSUER", &provider.issuer),
            ("BOP_OIDC_CLIENT_ID", CLIENT_ID),
        ],
    )
    .await;
    let version = "/api/v1/version";

    let (status, _) = get_status(&app, version, Some(&provider.token(&json!({})))).await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = get_status(&app, version, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    for (case, patch) in [
        ("expired", json!({ "exp": now() - 3600 })),
        ("another client", json!({ "aud": "other-app" })),
        ("another issuer", json!({ "iss": "https://evil.example/" })),
    ] {
        let (status, _) = get_status(&app, version, Some(&provider.token(&patch))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{case}");
    }

    // Signed with a shared secret instead of the provider's key.
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(KID.to_owned());
    let forged = jsonwebtoken::encode(
        &header,
        &json!({ "iss": provider.issuer, "aud": CLIENT_ID, "sub": "x", "exp": now() + 300 }),
        &EncodingKey::from_secret(b"guess"),
    )
    .unwrap();
    let (status, _) = get_status(&app, version, Some(&forged)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "HS256");

    // `alg: none` with the signature stripped.
    let valid = provider.token(&json!({}));
    let payload = valid.split('.').nth(1).unwrap();
    let unsigned = format!("eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.{payload}.");
    let (status, _) = get_status(&app, version, Some(&unsigned)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "alg none");

    // A tampered payload fails the signature.
    let mut parts: Vec<&str> = valid.split('.').collect();
    let other = provider.token(&json!({ "sub": "admin" }));
    parts[1] = other.split('.').nth(1).unwrap();
    let (status, _) = get_status(&app, version, Some(&parts.join("."))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "tampered");
}

#[tokio::test]
async fn groups_restrict_users_and_the_api_token_still_works() {
    let provider = Provider::start().await;
    let root = tempfile::tempdir().unwrap();
    let jwks = format!("{}jwks/", provider.issuer);
    let app = app(
        root.path(),
        &[
            ("BOP_OIDC_ISSUER", &provider.issuer),
            ("BOP_OIDC_CLIENT_ID", CLIENT_ID),
            ("BOP_OIDC_JWKS_URL", &jwks),
            ("BOP_OIDC_GROUPS", "bop-admins, bop-users"),
            ("BOP_API_TOKEN", "script-token"),
        ],
    )
    .await;
    let version = "/api/v1/version";

    let member = provider.token(&json!({}));
    assert_eq!(
        get_status(&app, version, Some(&member)).await.0,
        StatusCode::OK
    );
    let outsider = provider.token(&json!({ "groups": ["guests"] }));
    assert_eq!(
        get_status(&app, version, Some(&outsider)).await.0,
        StatusCode::FORBIDDEN
    );
    let no_groups = provider.token(&json!({ "groups": null }));
    assert_ne!(
        get_status(&app, version, Some(&no_groups)).await.0,
        StatusCode::OK
    );

    assert_eq!(
        get_status(&app, version, Some("script-token")).await.0,
        StatusCode::OK
    );
    assert_eq!(
        get_status(&app, version, Some("wrong-token")).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn unknown_signing_keys_are_rejected() {
    let provider = Provider::start().await;
    let root = tempfile::tempdir().unwrap();
    let app = app(
        root.path(),
        &[
            ("BOP_OIDC_ISSUER", &provider.issuer),
            ("BOP_OIDC_CLIENT_ID", CLIENT_ID),
        ],
    )
    .await;
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("rotated-away".to_owned());
    let token = jsonwebtoken::encode(
        &header,
        &json!({ "iss": provider.issuer, "aud": CLIENT_ID, "sub": "x", "exp": now() + 300 }),
        &provider.key,
    )
    .unwrap();
    assert_eq!(
        get_status(&app, "/api/v1/version", Some(&token)).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn the_sign_in_description_needs_no_credentials() {
    let provider = Provider::start().await;
    let root = tempfile::tempdir().unwrap();
    let app = app(
        root.path(),
        &[
            ("BOP_OIDC_ISSUER", &provider.issuer),
            ("BOP_OIDC_CLIENT_ID", CLIENT_ID),
        ],
    )
    .await;
    let (status, body) = get_status(&app, "/api/v1/auth", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["required"], true);
    assert_eq!(body["api_token"], false);
    assert_eq!(body["oidc"]["issuer"], provider.issuer.as_str());
    assert_eq!(body["oidc"]["client_id"], CLIENT_ID);
    assert!(body["oidc"]["scopes"].as_str().unwrap().contains("openid"));
}

#[test]
fn an_open_server_must_be_asked_for_outside_loopback() {
    let settings = |vars: &'static [(&'static str, &'static str)]| {
        bop_server::settings::Settings::from_vars(move |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        })
    };
    assert!(settings(&[]).is_ok(), "loopback by default");
    let error = settings(&[("BOP_LISTEN", "0.0.0.0:8080")]).unwrap_err();
    assert!(error.contains("without authentication"), "{error}");
    assert!(
        settings(&[
            ("BOP_LISTEN", "0.0.0.0:8080"),
            ("BOP_ALLOW_ANONYMOUS", "true")
        ])
        .is_ok()
    );
    assert!(settings(&[("BOP_LISTEN", "0.0.0.0:8080"), ("BOP_API_TOKEN", "t")]).is_ok());
    assert!(settings(&[("BOP_OIDC_ISSUER", "https://auth.example/")]).is_err());
    assert!(
        settings(&[
            ("BOP_OIDC_ISSUER", "auth.example"),
            ("BOP_OIDC_CLIENT_ID", "c")
        ])
        .is_err()
    );
}

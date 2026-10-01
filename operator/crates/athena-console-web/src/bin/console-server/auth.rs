//! OIDC bearer authentication for the console backend.
//!
//! Validates Keycloak-issued JWT access tokens against the realm JWKS and maps
//! realm roles to console roles (`admin`, `viewer`) via `OIDC_ROLE_MAPPINGS`.
//!
//! Enforcement posture:
//! - `POST /mcp` and mutating `/api/*` methods (POST/PUT) require a valid token.
//! - Read-only `/api/*` GETs stay open; the write surface is the research
//!   record (ResearchReport specs) and MCP can reach it.
//! - If `OIDC_ISSUER_URL` is unset, auth is disabled entirely (local dev).
//!
//! MCP authorization-spec posture (2026-07-28, RFC 9728): 401 responses carry
//! `WWW-Authenticate: Bearer resource_metadata=…`, and the resource metadata
//! document is served at `GET /.well-known/oauth-protected-resource`.

use axum::Json;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use parking_lot::RwLock;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Role resolved from a validated token. `Admin` may write; `Viewer` may call
/// read-only MCP tools.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Admin,
    Viewer,
}

/// Validated bearer claims attached to the request.
#[derive(Clone, Debug)]
pub struct Auth {
    pub role: Role,
}

#[derive(Clone)]
pub struct AuthState {
    issuer: String,
    client_id: String,
    /// console role -> token role strings, e.g. admin -> ["realm:admin"].
    role_mappings: Arc<HashMap<String, Vec<String>>>,
    jwks: Arc<RwLock<JwksCache>>,
    http: reqwest::Client,
}

struct JwksCache {
    keys: Vec<(String, jsonwebtoken::DecodingKey)>,
    fetched_at: Option<Instant>,
}

impl AuthState {
    /// Build from env. Returns `None` (auth disabled) when `OIDC_ISSUER_URL`
    /// is unset; errors when it is set but unusable.
    pub async fn from_env() -> anyhow::Result<Option<Self>> {
        let Ok(issuer) = std::env::var("OIDC_ISSUER_URL") else {
            eprintln!("auth: OIDC_ISSUER_URL unset; bearer auth DISABLED (dev mode)");
            return Ok(None);
        };
        let issuer = issuer.trim_end_matches('/').to_string();
        let client_id =
            std::env::var("OIDC_CLIENT_ID").unwrap_or_else(|_| "athena-console".to_string());
        let role_mappings: HashMap<String, Vec<String>> =
            match std::env::var("OIDC_ROLE_MAPPINGS") {
                Ok(raw) => serde_json::from_str(&raw)
                    .map_err(|e| anyhow::anyhow!("OIDC_ROLE_MAPPINGS is not valid JSON: {e}"))?,
                Err(_) => HashMap::from([
                    ("admin".into(), vec!["realm:admin".into()]),
                    ("viewer".into(), vec!["realm:viewer".into()]),
                ]),
            };
        let state = Self {
            issuer,
            client_id,
            role_mappings: Arc::new(role_mappings),
            jwks: Arc::new(RwLock::new(JwksCache {
                keys: Vec::new(),
                fetched_at: None,
            })),
            http: reqwest::Client::new(),
        };
        state.refresh_jwks().await?;
        eprintln!(
            "auth: bearer auth enabled; issuer={} client_id={}",
            state.issuer, state.client_id
        );
        Ok(Some(state))
    }

    fn jwks_url(&self) -> String {
        format!("{}/protocol/openid-connect/certs", self.issuer)
    }

    async fn refresh_jwks(&self) -> anyhow::Result<()> {
        let doc: Value = self
            .http
            .get(self.jwks_url())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let mut keys = Vec::new();
        for jwk in doc["keys"].as_array().into_iter().flatten() {
            let (Some(kid), Some(n), Some(e)) = (
                jwk["kid"].as_str(),
                jwk["n"].as_str(),
                jwk["e"].as_str(),
            ) else {
                continue;
            };
            if jwk["kty"].as_str() != Some("RSA") {
                continue;
            }
            if let Ok(key) = jsonwebtoken::DecodingKey::from_rsa_components(n, e) {
                keys.push((kid.to_string(), key));
            }
        }
        if keys.is_empty() {
            anyhow::bail!("JWKS at {} contained no usable RSA keys", self.jwks_url());
        }
        let mut cache = self.jwks.write();
        cache.keys = keys;
        cache.fetched_at = Some(Instant::now());
        Ok(())
    }

    /// Validate a bearer token and resolve the console role.
    pub async fn validate(&self, token: &str) -> Result<Auth, StatusCode> {
        let header = jsonwebtoken::decode_header(token).map_err(|_| StatusCode::UNAUTHORIZED)?;
        let kid = header.kid.ok_or(StatusCode::UNAUTHORIZED)?;

        // Refresh opportunistically (hourly, or when the kid is unknown) so
        // Keycloak key rotation does not strand the server.
        let needs_refresh = {
            let cache = self.jwks.read();
            cache
                .fetched_at
                .is_none_or(|t| t.elapsed() > Duration::from_secs(3600))
                || !cache.keys.iter().any(|(k, _)| k == &kid)
        };
        if needs_refresh {
            let _ = self.refresh_jwks().await;
        }
        let key = {
            let cache = self.jwks.read();
            cache
                .keys
                .iter()
                .find(|(k, _)| k == &kid)
                .map(|(_, k)| k.clone())
        }
        .ok_or(StatusCode::UNAUTHORIZED)?;

        // Keycloak puts audience in `aud`, but tokens for this client may only
        // carry `account`; accept the client id via `aud` or `azp`.
        let mut validation = jsonwebtoken::Validation::new(header.alg);
        validation.set_issuer(&[&self.issuer]);
        validation.validate_aud = false;
        validation.set_required_spec_claims(&["exp", "iat", "iss"]);
        let data = jsonwebtoken::decode::<Value>(token, &key, &validation)
            .map_err(|_| StatusCode::UNAUTHORIZED)?;
        let claims = data.claims;

        let audience_ok = claims["aud"]
            .as_array()
            .map(|a| a.iter().any(|v| v.as_str() == Some(&self.client_id)))
            .or_else(|| claims["aud"].as_str().map(|s| s == self.client_id))
            .unwrap_or(false);
        if !audience_ok {
            return Err(StatusCode::UNAUTHORIZED);
        }

        let mut token_roles: Vec<String> = Vec::new();
        if let Some(arr) = claims["realm_access"]["roles"].as_array() {
            token_roles.extend(arr.iter().filter_map(|r| r.as_str().map(str::to_string)));
        }
        if let Some(arr) = claims["roles"].as_array() {
            token_roles.extend(arr.iter().filter_map(|r| r.as_str().map(str::to_string)));
        }
        let has = |wanted: &str| {
            token_roles
                .iter()
                .any(|r| r == wanted || r == wanted.strip_prefix("realm:").unwrap_or(wanted))
        };
        let role = if self
            .role_mappings
            .get("admin")
            .is_some_and(|rs| rs.iter().any(|r| has(r)))
        {
            Role::Admin
        } else if self
            .role_mappings
            .get("viewer")
            .is_some_and(|rs| rs.iter().any(|r| has(r)))
        {
            Role::Viewer
        } else {
            return Err(StatusCode::FORBIDDEN);
        };
        Ok(Auth { role })
    }

    fn challenge(&self) -> String {
        format!(
            "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
            self.issuer_resource()
        )
    }

    /// Canonical resource identifier for this server (RFC 8707 §2). The MCP
    /// endpoint URL is the resource; clients must request tokens for it.
    fn issuer_resource(&self) -> String {
        std::env::var("OIDC_RESOURCE_URL")
            .unwrap_or_else(|_| "https://athena-console.casazza.io".to_string())
            .trim_end_matches('/')
            .to_string()
    }

    /// RFC 9728 Protected Resource Metadata.
    pub fn protected_resource_metadata(&self) -> Value {
        json!({
            "resource": self.issuer_resource(),
            "authorization_servers": [self.issuer],
            "scopes_supported": ["openid", "profile"],
            "bearer_methods_supported": ["header"],
        })
    }
}

/// Extract and validate the bearer token; 401 with the RFC 9728 challenge on
/// failure. No-op when auth is disabled (dev).
pub async fn require_auth(
    State(state): State<Option<AuthState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(state) = state else {
        return next.run(req).await;
    };
    let token = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").map(str::trim));
    let Some(token) = token else {
        return (
            StatusCode::UNAUTHORIZED,
            [(WWW_AUTHENTICATE, state.challenge())],
            "missing bearer token",
        )
            .into_response();
    };
    match state.validate(token).await {
        Ok(auth) => {
            req.extensions_mut().insert(auth);
            next.run(req).await
        }
        Err(status) => (
            status,
            [(WWW_AUTHENTICATE, state.challenge())],
            "invalid or insufficient token",
        )
            .into_response(),
    }

}

/// Read the validated auth from request extensions (set by `require_auth`).
pub fn request_auth(req: &Request) -> Option<&Auth> {
    req.extensions().get::<Auth>()
}

/// `GET /.well-known/oauth-protected-resource` (RFC 9728). 404 when auth is
/// disabled.
pub async fn protected_resource_metadata(
    State(state): State<Option<AuthState>>,
) -> Response {
    match state {
        Some(s) => Json(s.protected_resource_metadata()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `GET /api/auth/config` — public, browser-facing OIDC parameters. The SPA
/// uses these to run Authorization Code + PKCE in-browser; nothing here is
/// secret (client is public, no secret exists client-side).
pub async fn config(State(state): State<Option<AuthState>>) -> Response {
    match state {
        Some(s) => Json(json!({
            "enabled": true,
            "issuer": s.issuer,
            "clientId": s.client_id,
            "resource": s.issuer_resource(),
        }))
        .into_response(),
        None => Json(json!({ "enabled": false })).into_response(),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_points_at_resource_metadata() {
        unsafe { std::env::set_var("OIDC_RESOURCE_URL", "https://example.test/") };
        // Build a minimal state without network access.
        let state = AuthState {
            issuer: "https://auth.example.test/realms/master".into(),
            client_id: "athena-console".into(),
            role_mappings: Arc::new(HashMap::from([(
                "admin".into(),
                vec!["realm:admin".into()],
            )])),
            jwks: Arc::new(RwLock::new(JwksCache {
                keys: Vec::new(),
                fetched_at: None,
            })),
            http: reqwest::Client::new(),
        };
        assert_eq!(
            state.challenge(),
            "Bearer resource_metadata=\"https://example.test/.well-known/oauth-protected-resource\""
        );
        let meta = state.protected_resource_metadata();
        assert_eq!(meta["resource"], "https://example.test");
        assert_eq!(
            meta["authorization_servers"][0],
            "https://auth.example.test/realms/master"
        );
        unsafe { std::env::remove_var("OIDC_RESOURCE_URL") };
    }
}

/// Offline contract tests for `validate()` with a fixture RSA key.
#[cfg(test)]
mod contract_tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header};
    use std::time::{SystemTime, UNIX_EPOCH};

    const TEST_N_B64: &str = include_str!("fixtures/rsa_n.b64");
    const TEST_KEY_PEM: &str = include_str!("fixtures/rsa_key.pem");

    fn test_state() -> AuthState {
        let key =
            jsonwebtoken::DecodingKey::from_rsa_components(TEST_N_B64.trim(), "AQAB").unwrap();
        AuthState {
            issuer: "https://auth.example.test/realms/master".into(),
            client_id: "athena-console".into(),
            role_mappings: Arc::new(HashMap::from([
                ("admin".into(), vec!["realm:admin".into()]),
                ("viewer".into(), vec!["realm:viewer".into()]),
            ])),
            jwks: Arc::new(RwLock::new(JwksCache {
                keys: vec![("test-key".into(), key)],
                fetched_at: Some(std::time::Instant::now()),
            })),
            http: reqwest::Client::new(),
        }
    }

    fn sign(claims: Value) -> String {
        let key = EncodingKey::from_rsa_pem(TEST_KEY_PEM.trim().as_bytes()).unwrap();
        let mut header = Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = Some("test-key".into());
        jsonwebtoken::encode(&header, &claims, &key).unwrap()
    }

    fn now_plus(secs: i64) -> u64 {
        (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + secs) as u64
    }

    fn claims_admin() -> Value {
        json!({
            "iss": "https://auth.example.test/realms/master",
            "sub": "user-1",
            "azp": "athena-console",
            "aud": ["athena-console"],
            "exp": now_plus(300),
            "iat": now_plus(-10),
            "realm_access": { "roles": ["realm:admin"] }
        })
    }

    fn claims_viewer() -> Value {
        json!({
            "iss": "https://auth.example.test/realms/master",
            "sub": "user-2",
            "azp": "athena-console",
            "aud": ["athena-console"],
            "exp": now_plus(300),
            "iat": now_plus(-10),
            "realm_access": { "roles": ["realm:viewer"] }
        })
    }
    #[tokio::test]
    async fn admin_token_maps_to_admin_role() {
        let state = test_state();
        let token = sign(claims_admin());
        let auth = state.validate(&token).await.unwrap();
        assert_eq!(auth.role, Role::Admin);
    }

    #[tokio::test]
    async fn viewer_token_maps_to_viewer_role() {
        let state = test_state();
        let token = sign(claims_viewer());
        let auth = state.validate(&token).await.unwrap();
        assert_eq!(auth.role, Role::Viewer);
    }

    #[tokio::test]
    async fn expired_token_is_unauthorized() {
        let state = test_state();
        let mut c = claims_admin();
        c["exp"] = json!(1);
        assert_eq!(
            state.validate(&sign(c)).await.unwrap_err(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn wrong_issuer_is_unauthorized() {
        let state = test_state();
        let mut c = claims_admin();
        c["iss"] = json!("https://evil.example.test/realms/master");
        assert_eq!(
            state.validate(&sign(c)).await.unwrap_err(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn foreign_audience_is_unauthorized() {
        let state = test_state();
        let mut c = claims_admin();
        c["aud"] = json!(["some-other-resource"]);
        assert_eq!(
            state.validate(&sign(c)).await.unwrap_err(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn token_without_any_role_is_forbidden() {
        let state = test_state();
        let mut c = claims_admin();
        c["realm_access"] = json!({ "roles": [] });
        assert_eq!(
            state.validate(&sign(c)).await.unwrap_err(),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn tampered_signature_is_unauthorized() {
        let state = test_state();
        let mut header = Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = Some("test-key".into());
        let mut token = jsonwebtoken::encode(&header, &claims_admin(), &EncodingKey::from_rsa_pem(TEST_KEY_PEM.trim().as_bytes()).unwrap()).unwrap();
        let idx = token.len() - 2;
        let bytes = unsafe { token.as_bytes_mut() };
        bytes[idx] = if bytes[idx] == b'A' { b'B' } else { b'A' };
        assert_eq!(
            state.validate(&token).await.unwrap_err(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn garbage_token_is_unauthorized_without_panic() {
        let state = test_state();
        assert_eq!(
            state.validate("not-a-jwt").await.unwrap_err(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn aud_string_form_is_accepted_for_client() {
        let state = test_state();
        let mut c = claims_viewer();
        c["aud"] = json!("athena-console");
        let auth = state.validate(&sign(c)).await.unwrap();
        assert_eq!(auth.role, Role::Viewer);
    }

    /// A no-TLS reqwest rejects `https` before it ever opens a socket, so the
    /// failure is visible offline: connecting to a closed port on 127.0.0.1
    /// must fail for want of a connection, NOT because the scheme is unusable.
    ///
    /// This is the regression guard for a real outage: with reqwest declared
    /// `default-features = false` and no `rustls-tls`, the backend built fine,
    /// all 12 contract tests passed (they validate against a local RSA fixture
    /// and never touch the network), and then the container refused to start
    /// with "invalid URL, scheme is not http" while fetching the JWKS. Every
    /// OIDC validation path was dead on arrival.
    #[tokio::test]
    async fn jwks_fetch_reaches_https() {
        let err = reqwest::Client::new()
            .get("https://127.0.0.1:1/protocol/openid-connect/certs")
            .send()
            .await
            .expect_err("nothing listens on port 1, so this must fail")
            .to_string();
        assert!(
            !err.contains("scheme is not http"),
            "reqwest has no TLS backend, so https (and therefore JWKS fetch) \
             cannot work: {err}. Check that the native target still enables \
             the `rustls-tls` feature on reqwest."
        );
    }
}

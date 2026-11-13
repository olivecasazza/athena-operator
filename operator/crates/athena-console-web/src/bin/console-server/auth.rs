//! Private resource-server boundary shared by REST and MCP.
//!
//! Production requires an explicit profile, issuer, resource audience and
//! namespace allowlist. Only an explicitly selected loopback development
//! listener may disable bearer authentication. Kubernetes RBAC remains the
//! backend's privilege ceiling, not a substitute for HTTP authorization.

use athena_console_web::access::{AccessPermissions, AccessSession};
use axum::Json;
use axum::extract::{Extension, Request, State};
use axum::http::StatusCode;
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use parking_lot::RwLock;
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
pub enum Permission {
    Research,
    Reports,
    Infrastructure,
}

#[derive(Debug)]
struct Profile {
    name: String,
    namespaces: Arc<[String]>,
}

static LOCAL_PROFILE: OnceLock<Arc<Profile>> = OnceLock::new();

/// Validated request capabilities and the server-owned namespace boundary.
#[derive(Clone, Debug)]
pub struct Auth {
    pub permissions: AccessPermissions,
    profile: Arc<Profile>,
}

impl Auth {
    pub fn allows(&self, permission: Permission) -> bool {
        match permission {
            Permission::Research => self.permissions.read_research,
            Permission::Reports => self.permissions.write_reports,
            Permission::Infrastructure => self.permissions.read_infrastructure,
        }
    }

    pub fn namespaces(&self) -> &[String] {
        &self.profile.namespaces
    }

    pub fn require_namespace(&self, namespace: &str) -> Result<(), (StatusCode, String)> {
        if self.namespaces().iter().any(|allowed| allowed == namespace) {
            Ok(())
        } else {
            Err((StatusCode::FORBIDDEN, "namespace is outside this access profile".into()))
        }
    }

    fn session(&self) -> AccessSession {
        AccessSession {
            profile: self.profile.name.clone(),
            namespaces: self.namespaces().to_vec(),
            permissions: self.permissions,
        }
    }

    fn local() -> Self {
        Self {
            permissions: AccessPermissions {
                read_research: true,
                write_reports: true,
                read_infrastructure: true,
            },
            profile: LOCAL_PROFILE.get_or_init(|| Arc::new(Profile {
                name: "local-development".into(),
                namespaces: vec!["apps".into()].into(),
            })).clone(),
        }
    }
}

#[derive(Clone)]
pub struct AuthState {
    issuer: String,
    client_id: String,
    resource: String,
    jwks_url: String,
    profile: Arc<Profile>,
    jwks: Arc<RwLock<JwksCache>>,
    http: reqwest::Client,
}

struct JwksCache {
    /// Pin each key to the algorithm published by the issuer.
    keys: Vec<(String, String, jsonwebtoken::DecodingKey)>,
    fetched_at: Option<Instant>,
}

fn required_env(name: &str) -> anyhow::Result<String> {
    let value = std::env::var(name)
        .map_err(|_| anyhow::anyhow!("{name} is required by the private access profile"))?;
    if value.trim().is_empty() {
        anyhow::bail!("{name} must not be empty");
    }
    Ok(value)
}

fn namespaces(raw: &str) -> anyhow::Result<Arc<[String]>> {
    let namespaces: Vec<String> = raw.split(',').map(|ns| ns.trim().to_string()).collect();
    if namespaces.is_empty() || namespaces.iter().any(|ns| {
        ns.is_empty() || ns.len() > 63
            || !ns.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            || !ns.ends_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            || !ns.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    }) {
        anyhow::bail!("ATHENA_NAMESPACES must be an explicit comma-separated namespace allowlist");
    }
    Ok(namespaces.into())
}

fn https_identifier(name: &str, value: String) -> anyhow::Result<String> {
    let url = reqwest::Url::parse(&value)?;
    if url.scheme() != "https" || url.host_str().is_none()
        || !url.username().is_empty() || url.password().is_some()
        || url.query().is_some() || url.fragment().is_some()
    {
        anyhow::bail!("{name} must be an HTTPS identifier without credentials, query or fragment");
    }
    Ok(value.trim_end_matches('/').to_string())
}

impl AuthState {
    pub async fn from_env() -> anyhow::Result<Option<Self>> {
        match std::env::var("ATHENA_AUTH_MODE").as_deref().unwrap_or("required") {
            "required" => {}
            "disabled-local" => {
                let addr: std::net::SocketAddr = std::env::var("ATHENA_CONSOLE_ADDR")
                    .unwrap_or_else(|_| "0.0.0.0:8080".into()).parse()?;
                if !addr.ip().is_loopback() {
                    anyhow::bail!("disabled-local authentication requires a loopback listener");
                }
                let scope = namespaces(&std::env::var("ATHENA_NAMESPACES")
                    .unwrap_or_else(|_| "apps".into()))?;
                LOCAL_PROFILE.set(Arc::new(Profile {
                    name: "local-development".into(), namespaces: scope,
                })).map_err(|_| anyhow::anyhow!("local access profile was already initialized"))?;
                eprintln!("auth: explicit loopback-only local development mode");
                return Ok(None);
            }
            _ => anyhow::bail!("ATHENA_AUTH_MODE must be required or disabled-local"),
        }
        let issuer = https_identifier("OIDC_ISSUER_URL", required_env("OIDC_ISSUER_URL")?)?;
        let resource = https_identifier("OIDC_RESOURCE_URL", required_env("OIDC_RESOURCE_URL")?)?;
        let jwks_url = std::env::var("OIDC_JWKS_URL")
            .unwrap_or_else(|_| format!("{issuer}/protocol/openid-connect/certs"));
        let keys_url = reqwest::Url::parse(&jwks_url)?;
        if !matches!(keys_url.scheme(), "http" | "https") || keys_url.host_str().is_none()
            || !keys_url.username().is_empty() || keys_url.password().is_some()
            || keys_url.query().is_some() || keys_url.fragment().is_some()
        {
            anyhow::bail!("OIDC_JWKS_URL must be an explicit HTTP(S) public-key endpoint");
        }
        let state = Self {
            issuer,
            resource,
            jwks_url,
            client_id: required_env("OIDC_CLIENT_ID")?,
            profile: Arc::new(Profile {
                name: required_env("ATHENA_ACCESS_PROFILE")?,
                namespaces: namespaces(&required_env("ATHENA_NAMESPACES")?)?,
            }),
            jwks: Arc::new(RwLock::new(JwksCache { keys: Vec::new(), fetched_at: None })),
            http: reqwest::Client::new(),
        };
        state.refresh_jwks().await?;
        eprintln!("auth: private profile={} issuer={} resource={}",
            state.profile.name, state.issuer, state.resource);
        Ok(Some(state))
    }

    async fn refresh_jwks(&self) -> anyhow::Result<()> {
        let doc: Value = self.http.get(&self.jwks_url).send().await?
            .error_for_status()?.json().await?;
        let mut keys = Vec::new();
        for jwk in doc["keys"].as_array().into_iter().flatten() {
            let (Some(kid), Some(n), Some(e), Some(alg)) = (
                jwk["kid"].as_str(), jwk["n"].as_str(),
                jwk["e"].as_str(), jwk["alg"].as_str(),
            ) else { continue; };
            if jwk["kty"].as_str() != Some("RSA")
                || jwk["use"].as_str().is_some_and(|usage| usage != "sig")
                || !matches!(alg, "RS256" | "RS384" | "RS512" | "PS256" | "PS384" | "PS512")
            {
                continue;
            }
            if let Ok(key) = jsonwebtoken::DecodingKey::from_rsa_components(n, e) {
                keys.push((kid.to_string(), alg.to_string(), key));
            }
        }
        if keys.is_empty() {
            anyhow::bail!("JWKS at {} contained no usable RSA signing keys", self.jwks_url);
        }
        let mut cache = self.jwks.write();
        cache.keys = keys;
        cache.fetched_at = Some(Instant::now());
        Ok(())
    }

    pub async fn validate(&self, token: &str) -> Result<Auth, StatusCode> {
        let header = jsonwebtoken::decode_header(token).map_err(|_| StatusCode::UNAUTHORIZED)?;
        let kid = header.kid.ok_or(StatusCode::UNAUTHORIZED)?;
        let needs_refresh = {
            let cache = self.jwks.read();
            cache.fetched_at.is_none_or(|t| t.elapsed() > Duration::from_secs(3600))
                || !cache.keys.iter().any(|(k, _, _)| k == &kid)
        };
        if needs_refresh {
            let _ = self.refresh_jwks().await;
        }
        let cache = self.jwks.read();
        let (_, advertised_alg, key) = cache.keys.iter()
            .find(|(k, _, _)| k == &kid).ok_or(StatusCode::UNAUTHORIZED)?;
        match advertised_alg.parse::<jsonwebtoken::Algorithm>() {
            Ok(expected) if expected == header.alg => {}
            _ => return Err(StatusCode::UNAUTHORIZED),
        }
        let mut validation = jsonwebtoken::Validation::new(header.alg);
        validation.set_issuer(&[&self.issuer]);
        validation.validate_aud = false;
        validation.set_required_spec_claims(&["exp", "iat", "iss", "sub"]);
        let claims = jsonwebtoken::decode::<Value>(token, key, &validation)
            .map_err(|_| StatusCode::UNAUTHORIZED)?.claims;
        let audience_ok = claims["aud"].as_array()
            .map(|aud| aud.iter().any(|v| v.as_str() == Some(&self.resource)))
            .or_else(|| claims["aud"].as_str().map(|aud| aud == self.resource))
            .unwrap_or(false);
        if !audience_ok || claims["sub"].as_str().is_none_or(|sub| sub.trim().is_empty()) {
            return Err(StatusCode::UNAUTHORIZED);
        }
        let mut permissions = AccessPermissions::default();
        for role in claims["resource_access"][&self.client_id]["roles"]
            .as_array().into_iter().flatten().filter_map(Value::as_str)
        {
            match role {
                "viewer" => permissions.read_research = true,
                "researcher" => {
                    permissions.read_research = true;
                    permissions.write_reports = true;
                }
                "operator" => {
                    permissions.read_research = true;
                    permissions.read_infrastructure = true;
                }
                "admin" => permissions = AccessPermissions {
                    read_research: true, write_reports: true, read_infrastructure: true,
                },
                _ => {}
            }
        }
        if !permissions.read_research {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(Auth { permissions, profile: self.profile.clone() })
    }

    fn challenge(&self) -> String {
        format!("Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"", self.resource)
    }

    pub fn protected_resource_metadata(&self) -> Value {
        json!({
            "resource": self.resource,
            "authorization_servers": [self.issuer],
            "scopes_supported": ["openid", "profile"],
            "bearer_methods_supported": ["header"],
        })
    }
}

pub async fn require_auth(
    State(state): State<Option<AuthState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let auth = if let Some(state) = state {
        let token = req.headers().get(AUTHORIZATION).and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer ").map(str::trim));
        let Some(token) = token else {
            return (StatusCode::UNAUTHORIZED,
                [(WWW_AUTHENTICATE, state.challenge())], "sign in to access Athena").into_response();
        };
        match state.validate(token).await {
            Ok(auth) => auth,
            Err(StatusCode::FORBIDDEN) => return (StatusCode::FORBIDDEN,
                "this account has no Athena access role").into_response(),
            Err(_) => return (StatusCode::UNAUTHORIZED,
                [(WWW_AUTHENTICATE, state.challenge())], "session is invalid or expired; sign in again").into_response(),
        }
    } else {
        Auth::local()
    };
    if !crate::api::request_allowed(&auth, req.method(), req.uri().path()) {
        return (StatusCode::FORBIDDEN,
            "this access profile does not permit that operation").into_response();
    }
    req.extensions_mut().insert(auth);
    next.run(req).await
}

pub fn request_auth(req: &Request) -> Option<&Auth> {
    req.extensions().get::<Auth>()
}

pub async fn session(Extension(auth): Extension<Auth>) -> Json<AccessSession> {
    Json(auth.session())
}

pub async fn protected_resource_metadata(State(state): State<Option<AuthState>>) -> Response {
    match state {
        Some(state) => Json(state.protected_resource_metadata()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn config(State(state): State<Option<AuthState>>) -> Response {
    match state {
        Some(state) => Json(json!({
            "enabled": true, "issuer": state.issuer,
            "clientId": state.client_id, "resource": state.resource,
        })).into_response(),
        None => Json(json!({ "enabled": false })).into_response(),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_points_at_resource_metadata() {
        let state = contract_tests::test_state();
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
    pub(super) fn test_state() -> AuthState {
        let key =
            jsonwebtoken::DecodingKey::from_rsa_components(TEST_N_B64.trim(), "AQAB").unwrap();
        AuthState {
            issuer: "https://auth.example.test/realms/master".into(),
            client_id: "athena-console".into(),
            resource: "https://example.test".into(),
            jwks_url: "https://auth.example.test/realms/master/protocol/openid-connect/certs".into(),
            profile: Arc::new(Profile {
                name: "test-private".into(),
                namespaces: vec!["apps".into()].into(),
            }),
            jwks: Arc::new(RwLock::new(JwksCache {
                keys: vec![("test-key".into(), "RS256".into(), key)],
                fetched_at: Some(std::time::Instant::now()),
            })),
            http: reqwest::Client::new(),
        }
    }

    /// Cross-family confusion: a forged token declaring `alg: HS256` must not be
    /// validated with the realm's RSA public key used as an HMAC secret. The
    /// pin rejects it; note jsonwebtoken rejects this one on its own too, so
    /// this test documents the defence rather than proving it — the
    /// same-family test below is the one that fails without the pin.
    #[tokio::test]
    async fn token_declaring_a_different_algorithm_is_unauthorized() {
        let state = test_state();
        let mut header = Header::new(jsonwebtoken::Algorithm::HS256);
        header.kid = Some("test-key".into());
        // Sign with the same bytes a naive implementation would treat as a
        // shared secret: the RSA public key in n/e form.
        let pem = TEST_KEY_PEM.trim();
        let hmac_key = pem.as_bytes();
        let forged =
            jsonwebtoken::encode(&header, &claims_admin(), &EncodingKey::from_secret(hmac_key))
                .unwrap();
        assert_eq!(
            state.validate(&forged).await.unwrap_err(),
            StatusCode::UNAUTHORIZED,
            "a token declaring HS256 was accepted against an RS256 realm key"
        );
    }

    /// Same-family algorithm substitution. The realm advertises RS256 for this
    /// key; a token declaring RS512 is signed with the same RSA private key, so
    /// jsonwebtoken accepts it — the key family matches and the signature is
    /// valid. That is not a forgery, but it is not the algorithm the issuer
    /// published, and accepting it means the realm's declared `alg` is advisory
    /// rather than enforced. This is the case the pin actually catches, and the
    /// reason the HS256 variant above is not the test to rely on: jsonwebtoken
    /// already rejects cross-family confusion on its own.
    #[tokio::test]
    async fn token_declaring_another_algorithm_in_the_same_family_is_unauthorized() {
        let state = test_state();
        let mut header = Header::new(jsonwebtoken::Algorithm::RS512);
        header.kid = Some("test-key".into());
        let token = jsonwebtoken::encode(
            &header,
            &claims_admin(),
            &EncodingKey::from_rsa_pem(TEST_KEY_PEM.trim().as_bytes()).unwrap(),
        )
        .unwrap();
        assert_eq!(
            state.validate(&token).await.unwrap_err(),
            StatusCode::UNAUTHORIZED,
            "accepted a token whose alg differs from the one the realm advertises"
        );
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
            "aud": ["https://example.test"],
            "exp": now_plus(300),
            "iat": now_plus(-10),
            "resource_access": { "athena-console": { "roles": ["admin"] } }
        })
    }

    fn claims_viewer() -> Value {
        json!({
            "iss": "https://auth.example.test/realms/master",
            "sub": "user-2",
            "azp": "athena-console",
            "aud": ["https://example.test"],
            "exp": now_plus(300),
            "iat": now_plus(-10),
            "resource_access": { "athena-console": { "roles": ["viewer"] } }
        })
    }
    #[tokio::test]
    async fn admin_token_grants_all_capabilities() {
        let state = test_state();
        let token = sign(claims_admin());
        let auth = state.validate(&token).await.unwrap();
        assert!(auth.permissions.read_research && auth.permissions.write_reports && auth.permissions.read_infrastructure);
    }

    #[tokio::test]
    async fn viewer_token_is_research_read_only() {
        let state = test_state();
        let token = sign(claims_viewer());
        let auth = state.validate(&token).await.unwrap();
        assert!(auth.permissions.read_research);
        assert!(!auth.permissions.write_reports && !auth.permissions.read_infrastructure);
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
        c["resource_access"] = json!({});
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
    async fn aud_string_form_is_accepted_for_resource() {
        let state = test_state();
        let mut c = claims_viewer();
        c["aud"] = json!("https://example.test");
        let auth = state.validate(&sign(c)).await.unwrap();
        assert!(auth.permissions.read_research && !auth.permissions.write_reports);
    }

    #[tokio::test]
    async fn realm_admin_does_not_grant_application_access() {
        let mut claims = claims_admin();
        claims["resource_access"] = json!({});
        claims["realm_access"] = json!({ "roles": ["admin"] });
        assert_eq!(test_state().validate(&sign(claims)).await.unwrap_err(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn missing_or_empty_subject_is_unauthorized() {
        for subject in [Value::Null, json!("")] {
            let mut claims = claims_admin();
            claims["sub"] = subject;
            assert_eq!(test_state().validate(&sign(claims)).await.unwrap_err(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn researcher_and_operator_capabilities_are_independent_and_union() {
        for (roles, writes, infrastructure) in [
            (json!(["researcher"]), true, false),
            (json!(["operator"]), false, true),
            (json!(["researcher", "operator"]), true, true),
        ] {
            let mut claims = claims_viewer();
            claims["resource_access"]["athena-console"]["roles"] = roles;
            let auth = test_state().validate(&sign(claims)).await.unwrap();
            assert!(auth.permissions.read_research);
            assert_eq!(auth.permissions.write_reports, writes);
            assert_eq!(auth.permissions.read_infrastructure, infrastructure);
        }
    }

    #[tokio::test]
    async fn private_router_denies_anonymous_reads_and_viewer_writes_in_both_transports() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, crate::app(Some(test_state()), "/nonexistent".into())).await.unwrap();
        });
        let client = reqwest::Client::new();
        for path in ["/api/snapshot", "/api/scheduling", "/api/reports/apps/report", "/mcp"] {
            assert_eq!(client.get(format!("{base}{path}")).send().await.unwrap().status(), StatusCode::UNAUTHORIZED);
        }
        let token = sign(claims_viewer());
        for path in ["/api/reports", "/api/reports/apps/report"] {
            let request = if path.ends_with("/report") {
                client.put(format!("{base}{path}"))
            } else {
                client.post(format!("{base}{path}"))
            };
            assert_eq!(request.bearer_auth(&token).json(&json!({})).send().await.unwrap().status(), StatusCode::FORBIDDEN);
        }
        for path in ["/api/scheduling", "/api/reports/another-namespace/report"] {
            assert_eq!(client.get(format!("{base}{path}")).bearer_auth(&token).send().await.unwrap().status(), StatusCode::FORBIDDEN);
        }
        assert_eq!(client.get(format!("{base}/api/manifest/apps/secrets/password"))
            .bearer_auth(&token).send().await.unwrap().status(), StatusCode::BAD_REQUEST);
        let session: AccessSession = client.get(format!("{base}/api/auth/session"))
            .bearer_auth(&token).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(session.namespaces, ["apps"]);
        assert!(!session.permissions.write_reports);
        let tools: Value = client.post(format!("{base}/mcp")).bearer_auth(&token)
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .send().await.unwrap().json().await.unwrap();
        let tools = tools["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|tool| tool["name"] == "list_experiments"));
        assert!(!tools.iter().any(|tool| matches!(tool["name"].as_str(), Some("create_report" | "update_report" | "get_scheduling"))));
        for tool in ["create_report", "update_report", "get_scheduling"] {
            let result: Value = client.post(format!("{base}/mcp")).bearer_auth(&token)
                .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":tool,"arguments":{}}}))
                .send().await.unwrap().json().await.unwrap();
            assert_eq!(result["result"]["isError"], true, "{tool}");
        }
        server.abort();
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

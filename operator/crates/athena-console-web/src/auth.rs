//! Browser-side OIDC Authorization Code + PKCE for the console frontend.
//!
//! The console's Keycloak client is a *public* client for browser use: this
//! bundle carries a PKCE verifier and nothing secret. The confidential client
//! secret stays in the `console-server` binary and is never shipped to wasm.
//!
//! Everything that carries a security property — verifier generation, the S256
//! challenge, percent-encoding, URL and form-body construction, token parsing,
//! expiry skew — is pure and host-testable. Only the `web_sys` calls that
//! genuinely need a browser (storage, `location`, `history`) are left thin.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Public OIDC parameters served by the backend at `GET /api/auth/config`.
///
/// That endpoint is unauthenticated by design: it carries no secret, only the
/// issuer, client id, and RFC 8707 resource identifier a browser needs to
/// start a public-client flow.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthConfig {
    pub enabled: bool,
    #[serde(default)]
    pub issuer: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub resource: String,
}

impl AuthConfig {
    /// Usable when auth is on and both endpoints are present. Anything else
    /// means "local dev / auth disabled": sign-in must be a no-op rather than a
    /// broken redirect.
    fn usable(&self) -> bool {
        self.enabled && !self.issuer.is_empty() && !self.client_id.is_empty()
    }
}

/// localStorage key holding the PKCE verifier between the authorize redirect
/// and the callback. Single-use and short-lived; an abandoned flow is simply
/// overwritten by the next attempt.
const VERIFIER_KEY: &str = "athena_console_pkce_verifier";
/// localStorage key holding the `state` echoed back on the callback, checked
/// to reject a forged callback. Single-use.
const STATE_KEY: &str = "athena_console_pkce_state";
/// localStorage keys for the persisted token: the value and its epoch expiry.
const TOKEN_KEY: &str = "athena_console_access_token";
const TOKEN_EXPIRY_KEY: &str = "athena_console_access_token_expires_at";
/// Set when the backend rejects our token, so the next resolve offers sign-in.
const REJECTED_KEY: &str = "athena_console_token_rejected";

/// RFC 7636 §4.1: the verifier is 43–128 characters from the unreserved set.
/// 32 random bytes base64url-encoded without padding gives exactly 43.
const VERIFIER_BYTES: usize = 32;
/// `state` entropy, 128 bits.
const STATE_BYTES: usize = 16;
/// Refresh a token this long before it actually expires, so a request is not
/// built with a token that dies in flight.
const EXPIRY_SKEW_SECONDS: u64 = 30;

/// The verifier and its derived S256 challenge for one authorization request.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

/// Generate a fresh PKCE pair from the OS CSPRNG.
///
/// `getrandom` is the platform CSPRNG on every target here (`js` backend under
/// wasm, `getrandom(2)` on the host). A predictable verifier would let anyone
/// who observes the authorize request mint a token for the signed-in account,
/// so this must never fall back to `Math::random`.
pub(crate) fn new_pkce() -> Result<Pkce, String> {
    let mut bytes = [0u8; VERIFIER_BYTES];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("no secure randomness available: {e}"))?;
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    Ok(Pkce { challenge: s256(&verifier), verifier })
}

/// Random `state` for CSRF protection on the callback, base64url-encoded.
pub(crate) fn new_state() -> Result<String, String> {
    let mut bytes = [0u8; STATE_BYTES];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("no secure randomness available: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// RFC 7636 §4.2: `BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`, unpadded.
fn s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Percent-encode everything outside the RFC 3986 unreserved set.
///
/// Stricter than `URLSearchParams`: a raw `+` in a form body decodes as a
/// space, and a raw `&` or `=` in a query value splits it, so encoding those
/// is what keeps a redirect URI containing `?x=1&y=2` from corrupting the
/// request.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// Append one `key=value` pair to a query string.
fn push_param(query: &mut String, key: &str, value: &str) {
    if !query.is_empty() {
        query.push('&');
    }
    query.push_str(key);
    query.push('=');
    query.push_str(&encode(value));
}

/// Build the Keycloak authorization URL for a public client.
pub(crate) fn authorize_url(
    config: &AuthConfig,
    redirect_uri: &str,
    pkce: &Pkce,
    state: &str,
) -> Option<String> {
    if !config.usable() {
        return None;
    }
    let mut url = format!(
        "{}/protocol/openid-connect/auth",
        config.issuer.trim_end_matches('/')
    );
    let mut query = String::new();
    push_param(&mut query, "client_id", &config.client_id);
    push_param(&mut query, "response_type", "code");
    push_param(&mut query, "scope", "openid profile");
    push_param(&mut query, "redirect_uri", redirect_uri);
    push_param(&mut query, "code_challenge", &pkce.challenge);
    push_param(&mut query, "code_challenge_method", "S256");
    push_param(&mut query, "state", state);
    // RFC 8707: bind the token to this resource server so the backend's
    // audience check and the minted token agree on what they protect.
    if !config.resource.is_empty() {
        push_param(&mut query, "resource", &config.resource);
    }
    url.push('?');
    url.push_str(&query);
    Some(url)
}

/// Form body for the public-client token exchange (no `client_secret`).
pub(crate) fn token_request_body(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    client_id: &str,
) -> String {
    let mut body = String::new();
    push_param(&mut body, "grant_type", "authorization_code");
    push_param(&mut body, "code", code);
    push_param(&mut body, "code_verifier", verifier);
    push_param(&mut body, "redirect_uri", redirect_uri);
    push_param(&mut body, "client_id", client_id);
    body
}

/// Minimal token response shape. We need the access token and its lifetime;
/// the refresh token is deliberately ignored — an unaided refresh from the
/// browser would mean storing a long-lived credential in localStorage.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// Outcome of parsing a token endpoint response body.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Token {
    pub access_token: String,
    /// Absolute expiry in epoch seconds, or `None` when the server omitted
    /// `expires_in` (treated as non-expiring; the next 401 forces a re-login).
    pub expires_at: Option<u64>,
}

pub(crate) fn parse_token(body: &str, now: u64) -> Result<Token, String> {
    let parsed: TokenResponse =
        serde_json::from_str(body).map_err(|e| format!("malformed token response: {e}"))?;
    if parsed.access_token.is_empty() {
        return Err("token response carried an empty access_token".into());
    }
    Ok(Token {
        access_token: parsed.access_token,
        expires_at: parsed.expires_in.map(|s| now.saturating_add(s)),
    })
}

/// A token is usable until shortly before it expires.
pub(crate) fn token_is_fresh(token: &Token, now: u64) -> bool {
    match token.expires_at {
        Some(exp) => now.saturating_add(EXPIRY_SKEW_SECONDS) < exp,
        None => true,
    }
}

/// Current epoch seconds, or 0 if the clock is unavailable.
pub(crate) fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Browser glue. Every entry point is compiled out off-browser: the `web_sys`
// imports panic on a non-wasm target rather than returning an error, so the
// host build must not reference them at all. This is what lets the pure logic
// above be unit-tested while the browser build keeps real DOM access.
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

#[cfg(not(target_arch = "wasm32"))]
fn storage() -> Option<web_sys::Storage> {
    None
}

fn read(key: &str) -> Option<String> {
    storage()?.get_item(key).ok().flatten().filter(|v| !v.is_empty())
}

fn write(key: &str, value: &str) {
    if let Some(s) = storage() {
        let _ = s.set_item(key, value);
    }
}

/// Take the single-use PKCE verifier for a token exchange. Removed on read, so
/// a replayed callback cannot reuse it.
pub(crate) fn take_verifier() -> Option<String> {
    let verifier = read(VERIFIER_KEY);
    drop_key(VERIFIER_KEY);
    verifier
}

fn drop_key(key: &str) {
    if let Some(s) = storage() {
        let _ = s.remove_item(key);
    }
}

pub(crate) fn save_pending(pkce: &Pkce, state: &str) {
    write(VERIFIER_KEY, &pkce.verifier);
    write(STATE_KEY, state);
}

/// The verifier is single-use: read removes it, so a replayed callback cannot
/// reuse it. `state` likewise, after a successful match.
pub(crate) fn take_pending() -> Option<(String, String)> {
    let verifier = read(VERIFIER_KEY)?;
    let state = read(STATE_KEY)?;
    drop_key(VERIFIER_KEY);
    drop_key(STATE_KEY);
    Some((verifier, state))
}

/// Discard a pending flow without completing it (mismatched `state`, or an
/// IdP-reported error).
pub(crate) fn discard_pending() {
    drop_key(VERIFIER_KEY);
    drop_key(STATE_KEY);
}

/// The current page URL, which doubles as the registered redirect target.
#[cfg(target_arch = "wasm32")]
pub(crate) fn current_url() -> Option<String> {
    web_sys::window()?.location().href().ok()
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn current_url() -> Option<String> {
    None
}

/// Strip the `?code=…&state=…` callback query so a reload does not replay the
/// code. `replaceState` leaves the console's own routing untouched.
#[cfg(target_arch = "wasm32")]
pub(crate) fn clear_callback_query() {
    if let Some(w) = web_sys::window()
        && let Ok(history) = w.history()
    {
        let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some("/"));
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn clear_callback_query() {}

/// A query parameter of the current URL, if present.
#[cfg(target_arch = "wasm32")]
fn query_param(name: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    web_sys::UrlSearchParams::new_with_str(search.strip_prefix('?')?)
        .ok()?
        .get(name)
}

#[cfg(not(target_arch = "wasm32"))]
fn query_param(_name: &str) -> Option<String> {
    None
}

/// `code` query parameter of the current URL, if present.
pub(crate) fn callback_code() -> Option<String> {
    query_param("code")
}

/// `state` query parameter of the current URL, if present.
pub(crate) fn callback_state() -> Option<String> {
    query_param("state")
}

/// Why the IdP refused the request, if it said: the human-readable
/// `error_description` when present, else the bare `error` code.
pub(crate) fn callback_error() -> Option<String> {
    query_param("error_description").or_else(|| query_param("error"))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn navigate_to(url: &str) {
    if let Some(w) = web_sys::window() {
        let _ = w.location().set_href(url);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn navigate_to(_url: &str) {}

pub(crate) fn store_token(token: &Token) {
    write(TOKEN_KEY, &token.access_token);
    if let Some(exp) = token.expires_at {
        write(TOKEN_EXPIRY_KEY, &exp.to_string());
    }
}

pub(crate) fn clear_token() {
    drop_key(TOKEN_KEY);
    drop_key(TOKEN_EXPIRY_KEY);
}

/// The bearer token to attach to a write request, or `None` when absent or
/// within the expiry skew. A token we cannot date is trusted and left to the
/// backend's 401 to reject.
pub(crate) fn bearer_token(now: u64) -> Option<String> {
    let token = read(TOKEN_KEY)?;
    if let Some(exp) = read(TOKEN_EXPIRY_KEY).and_then(|v| v.parse::<u64>().ok())
        && !token_is_fresh(&Token { access_token: String::new(), expires_at: Some(exp) }, now)
    {
        return None;
    }
    Some(token)
}

/// Record that the backend rejected our token (401). The next auth resolve
/// turns this into an `Expired` phase with a fresh sign-in link, so a stale
/// token is not retried and the user is told what to do.
pub(crate) fn mark_rejected() {
    write(REJECTED_KEY, "1");
}

/// Whether a token has been rejected since the last check.
pub(crate) fn take_rejected() -> bool {
    let rejected = read(REJECTED_KEY).is_some();
    drop_key(REJECTED_KEY);
    rejected
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    /// RFC 7636 Appendix B verifier and its documented challenge.
    const RFC_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const RFC_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    fn enabled() -> AuthConfig {
        AuthConfig {
            enabled: true,
            issuer: "https://auth.example.test/realms/master".into(),
            client_id: "athena-console".into(),
            resource: "https://athena-console.example.test".into(),
        }
    }

    fn parse_query(url: &str) -> HashMap<String, String> {
        let query = url.split_once('?').expect("url has no query").1;
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .map(|(k, v)| (k.to_string(), percent_decode(v)))
            .collect()
    }

    fn percent_decode(value: &str) -> String {
        let bytes = value.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn challenge_matches_rfc7636_worked_example() {
        assert_eq!(s256(RFC_VERIFIER), RFC_CHALLENGE);
    }

    #[test]
    fn verifier_is_within_rfc7636_length_bounds() {
        let pkce = new_pkce().unwrap();
        assert!(
            (43..=128).contains(&pkce.verifier.len()),
            "verifier length {} outside 43..=128",
            pkce.verifier.len()
        );
    }

    #[test]
    fn verifier_uses_only_unreserved_characters() {
        let pkce = new_pkce().unwrap();
        assert!(
            pkce.verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')),
            "verifier has reserved characters: {}",
            pkce.verifier
        );
    }

    #[test]
    fn challenge_is_unpadded_base64url_of_32_bytes() {
        let pkce = new_pkce().unwrap();
        assert_eq!(pkce.challenge.len(), 43);
        assert!(!pkce.challenge.contains('='));
        assert!(!pkce.challenge.contains('+') && !pkce.challenge.contains('/'));
    }

    #[test]
    fn fresh_challenges_are_unguessable_and_distinct() {
        // 64 draws: a constant or low-entropy source collides or repeats here.
        let set: HashSet<String> = (0..64)
            .map(|_| new_pkce().unwrap().challenge)
            .collect();
        assert_eq!(set.len(), 64, "challenges repeated; entropy source is broken");
    }

    #[test]
    fn fresh_verifiers_are_distinct() {
        let set: HashSet<String> = (0..64).map(|_| new_pkce().unwrap().verifier).collect();
        assert_eq!(set.len(), 64);
    }

    #[test]
    fn challenge_is_derived_from_verifier_not_independent() {
        let pkce = new_pkce().unwrap();
        assert_eq!(pkce.challenge, s256(&pkce.verifier));
    }

    #[test]
    fn state_is_random_and_url_safe() {
        let set: HashSet<String> = (0..64).map(|_| new_state().unwrap()).collect();
        assert_eq!(set.len(), 64);
        assert!(
            new_state()
                .unwrap()
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        );
    }

    #[test]
    fn authorize_url_is_none_when_auth_disabled() {
        let pkce = new_pkce().unwrap();
        assert!(
            authorize_url(
                &AuthConfig::default(),
                "https://c.test/",
                &pkce,
                "st"
            )
            .is_none()
        );
    }

    #[test]
    fn authorize_url_is_none_when_issuer_or_client_missing() {
        let pkce = new_pkce().unwrap();
        let mut no_issuer = enabled();
        no_issuer.issuer = String::new();
        assert!(
            authorize_url(&no_issuer, "https://c.test/", &pkce, "st").is_none(),
            "empty issuer must not produce a relative redirect"
        );

        let mut no_client = enabled();
        no_client.client_id = String::new();
        assert!(authorize_url(&no_client, "https://c.test/", &pkce, "st").is_none());
    }

    #[test]
    fn authorize_url_targets_the_issuer_authorize_endpoint() {
        let pkce = new_pkce().unwrap();
        let mut trailing = enabled();
        trailing.issuer = "https://auth.example.test/realms/master/".into();
        let url = authorize_url(&trailing, "https://c.test/", &pkce, "st").unwrap();
        assert!(
            url.starts_with("https://auth.example.test/realms/master/protocol/openid-connect/auth?"),
            "unexpected authorize endpoint: {url}"
        );
    }

    #[test]
    fn authorize_url_carries_pkce_resource_and_state() {
        let pkce = new_pkce().unwrap();
        let url = authorize_url(
            &enabled(),
            "https://athena-console.example.test/",
            &pkce,
            "state-123",
        )
        .unwrap();
        let parsed = parse_query(&url);

        assert_eq!(parsed.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(
            parsed.get("client_id").map(String::as_str),
            Some("athena-console")
        );
        assert_eq!(
            parsed.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(
            parsed.get("code_challenge").map(String::as_str),
            Some(pkce.challenge.as_str())
        );
        assert_eq!(parsed.get("state").map(String::as_str), Some("state-123"));
        assert_eq!(
            parsed.get("resource").map(String::as_str),
            Some("https://athena-console.example.test")
        );
        assert_eq!(
            parsed.get("redirect_uri").map(String::as_str),
            Some("https://athena-console.example.test/")
        );
    }

    #[test]
    fn authorize_url_never_carries_a_client_secret() {
        let pkce = new_pkce().unwrap();
        let url = authorize_url(&enabled(), "https://c.test/", &pkce, "st").unwrap();
        assert!(!url.contains("client_secret"));
        // The verifier must never travel on the authorize leg.
        assert!(!url.contains(&pkce.verifier));
    }

    #[test]
    fn authorize_url_omits_resource_when_unset() {
        let pkce = new_pkce().unwrap();
        let mut no_resource = enabled();
        no_resource.resource = String::new();
        let url = authorize_url(&no_resource, "https://c.test/", &pkce, "st").unwrap();
        assert!(!parse_query(&url).contains_key("resource"));
    }

    #[test]
    fn authorize_url_percent_encodes_redirect_uri() {
        let pkce = new_pkce().unwrap();
        let url = authorize_url(&enabled(), "https://c.test/app?x=1&y=2", &pkce, "st").unwrap();
        // A raw `&` inside redirect_uri would split the query and corrupt it.
        assert!(!url.contains("redirect_uri=https://c.test/app?x=1&y=2"));
        assert_eq!(
            parse_query(&url).get("redirect_uri").map(String::as_str),
            Some("https://c.test/app?x=1&y=2")
        );
    }

    #[test]
    fn authorize_url_encodes_space_in_scope() {
        let pkce = new_pkce().unwrap();
        let url = authorize_url(&enabled(), "https://c.test/", &pkce, "st").unwrap();
        assert_eq!(
            parse_query(&url).get("scope").map(String::as_str),
            Some("openid profile")
        );
    }

    #[test]
    fn token_request_body_never_carries_a_secret() {
        let body = token_request_body("c0de", "v3rifier", "https://c.test/", "athena-console");
        assert!(!body.contains("client_secret"));
        assert!(body.contains("grant_type=authorization_code"));
        assert!(body.contains("code=c0de"));
        assert!(body.contains("code_verifier=v3rifier"));
        assert!(body.contains("client_id=athena-console"));
    }

    #[test]
    fn token_request_body_escapes_reserved_characters() {
        let body = token_request_body("a+b/c&d", "v+r", "https://c.test/?a=1", "athena-console");
        let parsed: HashMap<&str, &str> = body
            .split('&')
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k, v))
            .collect();
        assert_eq!(parsed.get("code").copied(), Some("a%2Bb%2Fc%26d"));
        assert_eq!(parsed.get("code_verifier").copied(), Some("v%2Br"));
        // A `+` must survive as %2B: raw it decodes as a space server-side.
        assert!(!body.contains("code_verifier=v+r"));
    }

    #[test]
    fn parse_token_reads_access_token_and_expiry() {
        let token = parse_token(
            r#"{"access_token":"at-1","expires_in":300,"refresh_token":"rt"}"#,
            1000,
        )
        .unwrap();
        assert_eq!(token.access_token, "at-1");
        assert_eq!(token.expires_at, Some(1300));
    }

    #[test]
    fn parse_token_accepts_missing_expiry_as_non_expiring() {
        let token = parse_token(r#"{"access_token":"at-1"}"#, 1000).unwrap();
        assert_eq!(token.expires_at, None);
        assert!(token_is_fresh(&token, 9_999_999));
    }

    #[test]
    fn parse_token_rejects_malformed_or_empty_body() {
        assert!(parse_token("not json", 0).is_err());
        assert!(parse_token("{}", 0).is_err());
        assert!(parse_token(r#"{"access_token":""}"#, 0).is_err());
        assert!(parse_token(r#"{"access_token":42}"#, 0).is_err());
    }

    #[test]
    fn token_is_fresh_until_just_before_expiry() {
        let token = parse_token(r#"{"access_token":"at","expires_in":300}"#, 1000).unwrap();
        assert!(token_is_fresh(&token, 1000));
        assert!(token_is_fresh(&token, 1240));
        // Within the skew window it is already stale.
        assert!(!token_is_fresh(&token, 1271));
        assert!(!token_is_fresh(&token, 5000));
    }

    #[test]
    fn now_seconds_is_after_2020() {
        assert!(now_seconds() > 1_577_836_800);
    }
}

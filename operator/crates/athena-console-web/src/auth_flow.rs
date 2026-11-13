//! Drives the browser half of the OIDC login: reads `/api/auth/config`,
//! runs the PKCE redirect, and completes the token exchange.
//!
//! The state machine is pure and unit-tested; the effectful edges (network,
//! `location`, storage) are injected so the transitions can be verified
//! without a browser.

use crate::auth::{self, AuthConfig, Pkce};
use crate::access::AccessSession;

/// Where the frontend is in the sign-in flow.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Phase {
    /// Checking `/api/auth/config`.
    Loading,
    /// Explicit loopback-only development profile.
    Disabled,
    /// No usable token; `SignIn` carries the authorize URL to navigate to.
    SignedOut { sign_in: Option<String> },
    /// Capabilities verified by the backend, not inferred from token presence.
    SignedIn { session: AccessSession },
    /// Authenticated identity without an assigned application role.
    Denied { message: String, sign_in: Option<String> },
    /// A stored token was rejected or expired mid-session.
    Expired { message: String, sign_in: Option<String> },
    /// The IdP or the exchange failed.
    Failed { message: String, sign_in: Option<String> },
}

/// A fresh stored credential still needs server verification; expiry is not
/// the same state as a visitor who has never signed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StoredSession {
    Missing,
    Expired,
    Verify,
}

pub(crate) fn stored_session(token: Option<&auth::Token>, now: u64) -> StoredSession {
    match token {
        None => StoredSession::Missing,
        Some(token) if auth::token_is_fresh(token, now) => StoredSession::Verify,
        Some(_) => StoredSession::Expired,
    }
}

impl Phase {
    pub(crate) fn can_read(&self) -> bool {
        matches!(self, Phase::Disabled)
            || matches!(self, Phase::SignedIn { session } if session.permissions.read_research)
    }

    pub(crate) fn can_write(&self) -> bool {
        matches!(self, Phase::Disabled)
            || matches!(self, Phase::SignedIn { session } if session.permissions.write_reports)
    }

    pub(crate) fn can_read_infrastructure(&self) -> bool {
        matches!(self, Phase::Disabled)
            || matches!(self, Phase::SignedIn { session } if session.permissions.read_infrastructure)
    }

    /// One line for the top bar: what the user is, and what to do next.
    pub(crate) fn notice(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Phase::Disabled => Some(("local development", "explicit loopback-only profile; no bearer required")),
            Phase::Expired { .. } => Some(("session expired", "sign in again; unsaved feedback is preserved")),
            Phase::Denied { .. } => Some(("access not assigned", "ask the owner for an Athena client role")),
            Phase::Failed { .. } => Some(("sign-in failed", "see the message; try again")),
            _ => None,
        }
    }
}

/// The sign-in URL to offer, if the flow can be started.
pub(crate) fn sign_in_url(phase: &Phase) -> Option<String> {
    match phase {
        Phase::SignedOut { sign_in } | Phase::Expired { sign_in, .. }
        | Phase::Denied { sign_in, .. } | Phase::Failed { sign_in, .. } => sign_in.clone(),
        _ => None,
    }
}

/// Start a sign-in: mint PKCE + state, persist them, and return the URL to
/// navigate to. `None` when the backend has no usable OIDC config.
pub(crate) fn begin(config: &AuthConfig, redirect_uri: &str) -> Option<String> {
    let pkce: Pkce = auth::new_pkce().ok()?;
    let state = auth::new_state().ok()?;
    let url = auth::authorize_url(config, redirect_uri, &pkce, &state)?;
    auth::save_pending(&pkce, &state, redirect_uri);
    Some(url)
}

/// Consume state and verifier together. Even a mismatched callback cannot
/// replay the pending request; a valid callback retains its verifier for exchange.
pub(crate) fn check_state(returned: &str) -> Result<auth::PendingLogin, String> {
    consume_pending(&mut auth::take_pending(), returned)
}

fn consume_pending(pending: &mut Option<auth::PendingLogin>, returned: &str) -> Result<auth::PendingLogin, String> {
    match pending.take() {
        Some(login) if login.state == returned => Ok(login),
        Some(_) => Err("sign-in state did not match; start sign-in again".into()),
        None => Err("no sign-in was in progress for this callback".into()),
    }
}

/// Human-readable failure text for a non-2xx token exchange.
pub(crate) fn token_error(status: u16, body: &str) -> String {
    // Keycloak reports the useful reason in error_description; fall back to
    // the raw body so an unexpected shape is still visible.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(desc) = v.get("error_description").and_then(|d| d.as_str()) {
            return desc.to_string();
        }
    }
    let body = body.trim();
    if body.is_empty() {
        format!("token exchange failed with HTTP {status}")
    } else {
        format!("token exchange failed with HTTP {status}: {body}")
    }
}

/// Whether a report write may proceed, and why not when it may not.
///
/// The blocker message is either the auth problem or, once signed in, the
/// spec's own validation problem — auth first, so the user is told what
/// actually needs fixing.
pub(crate) fn save_gate(
    phase: &Phase,
    spec_problem: Option<String>,
    dirty: bool,
    busy: bool,
) -> (bool, Option<String>) {
    let problem = if phase.can_write() {
        spec_problem
    } else if matches!(phase, Phase::SignedIn { .. }) {
        Some("read-only access; a researcher or admin role is required to save reports".into())
    } else {
        Some("sign in to save reports".to_string())
    };
    (dirty && problem.is_none() && !busy, problem)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> AuthConfig {
        AuthConfig {
            enabled: true,
            issuer: "https://auth.example.test/realms/master".into(),
            client_id: "athena-console".into(),
            resource: "https://athena-console.example.test".into(),
        }
    }

    fn signed_in() -> Phase {
        Phase::SignedIn { session: AccessSession {
            profile: "test".into(),
            namespaces: vec!["apps".into()],
            permissions: crate::access::AccessPermissions {
                read_research: true, write_reports: true, read_infrastructure: false,
            },
        }}
    }

    #[test]
    fn expired_credentials_are_not_treated_as_a_first_time_visitor() {
        let token = auth::Token { access_token: "opaque-credential".into(), expires_at: Some(100) };
        assert_eq!(stored_session(None, 75), StoredSession::Missing);
        assert_eq!(stored_session(Some(&token), 50), StoredSession::Verify);
        assert_eq!(stored_session(Some(&token), 75), StoredSession::Expired);
    }

    #[test]
    fn callback_verifier_survives_validation_but_cannot_be_replayed() {
        let login = || auth::PendingLogin {
            verifier: "original-verifier".into(), state: "expected-state".into(),
            redirect_uri: "https://console.test/review?candidate=run-1".into(),
        };
        let mut pending = Some(login());
        let verified = consume_pending(&mut pending, "expected-state").unwrap();
        assert_eq!(verified.verifier, "original-verifier");
        assert_eq!(verified.redirect_uri, "https://console.test/review?candidate=run-1");
        assert!(consume_pending(&mut pending, "expected-state").is_err());
        let mut pending = Some(login());
        assert!(consume_pending(&mut pending, "forged-state").is_err());
        assert!(consume_pending(&mut pending, "expected-state").is_err());
    }

    #[test]
    fn viewer_keeps_research_access_without_saves_or_infrastructure() {
        let mut phase = signed_in();
        if let Phase::SignedIn { session } = &mut phase {
            session.permissions.write_reports = false;
        }
        assert!(phase.can_read());
        assert!(!phase.can_write());
        assert!(!phase.can_read_infrastructure());
        assert!(!save_gate(&phase, None, true, false).0);
    }

    #[test]
    fn disabled_phase_can_write_because_backend_is_open() {
        assert!(Phase::Disabled.can_write());
        assert!(Phase::Disabled.can_read_infrastructure());
    }

    #[test]
    fn signed_out_cannot_write_but_offers_sign_in() {
        let phase = Phase::SignedOut { sign_in: Some("https://auth.example.test/go".into()) };
        assert!(!phase.can_write());
        assert_eq!(
            sign_in_url(&phase),
            Some("https://auth.example.test/go".to_string())
        );
    }

    #[test]
    fn signed_in_can_write_and_is_quiet() {
        assert!(signed_in().can_write());
        assert_eq!(sign_in_url(&signed_in()), None);
    }

    #[test]
    fn expired_and_failed_explain_the_recovery() {
        let expired = Phase::Expired {
            message: "token rejected".into(),
            sign_in: Some("https://auth.example.test/go".into()),
        };
        assert!(!expired.can_write());
        assert_eq!(expired.notice().map(|(a, _)| a), Some("session expired"));
        assert!(sign_in_url(&expired).is_some());

        let failed = Phase::Failed {
            message: "nope".into(),
            sign_in: None,
        };
        assert_eq!(failed.notice().map(|(a, _)| a), Some("sign-in failed"));
        assert_eq!(sign_in_url(&failed), None);
    }

    #[test]
    fn loading_is_neither_signed_in_nor_offering_sign_in() {
        assert!(!Phase::Loading.can_write());
        assert_eq!(sign_in_url(&Phase::Loading), None);
        assert_eq!(Phase::Loading.notice(), None);
    }

    #[test]
    fn begin_returns_none_when_backend_auth_is_disabled() {
        assert!(begin(&AuthConfig::default(), "https://c.test/").is_none());
    }

    #[test]
    fn begin_builds_a_pkce_url_for_a_usable_config() {
        // Off-browser `begin` still mints PKCE and returns a URL; only the
        // storage write inside `save_pending` no-ops.
        let url = begin(&config(), "https://c.test/").expect("usable config");
        let query = url.split_once('?').expect("query").1;
        assert!(query.contains("code_challenge_method=S256"), "{query}");
        assert!(query.contains("state="), "{query}");
        assert!(!url.contains("client_secret"), "{url}");
    }

    #[test]
    fn token_error_surfaces_keycloak_description() {
        let body = r#"{"error":"invalid_grant","error_description":"Code already used"}"#;
        assert_eq!(token_error(400, body), "Code already used");
    }

    #[test]
    fn token_error_falls_back_to_body_then_status() {
        assert_eq!(
            token_error(500, "upstream exploded"),
            "token exchange failed with HTTP 500: upstream exploded"
        );
    }

    #[test]
    fn signed_in_user_can_save_a_valid_dirty_draft() {
        let (ok, problem) = save_gate(&signed_in(), None, true, false);
        assert!(ok);
        assert_eq!(problem, None);
    }

    #[test]
    fn signed_out_user_cannot_save_and_is_told_to_sign_in() {
        let out = Phase::SignedOut { sign_in: Some("https://auth.test/go".into()) };
        let (ok, problem) = save_gate(&out, None, true, false);
        assert!(!ok);
        assert_eq!(problem.as_deref(), Some("sign in to save reports"));
    }

    #[test]
    fn expired_and_failed_phases_block_the_write() {
        let expired = Phase::Expired {
            message: "token rejected".into(),
            sign_in: Some("https://auth.test/go".into()),
        };
        assert!(!save_gate(&expired, None, true, false).0);

        let failed = Phase::Failed { message: "nope".into(), sign_in: None };
        assert!(!save_gate(&failed, None, true, false).0);
    }

    #[test]
    fn auth_blocker_hides_the_spec_problem() {
        // Signed out, the spec problem is irrelevant until you can sign in.
        let out = Phase::SignedOut { sign_in: None };
        let (_, problem) = save_gate(&out, Some("name is required".into()), true, false);
        assert_eq!(problem.as_deref(), Some("sign in to save reports"));
    }

    #[test]
    fn disabled_auth_still_saves_for_local_dev() {
        let (ok, problem) = save_gate(&Phase::Disabled, None, true, false);
        assert!(ok);
        assert_eq!(problem, None);
    }

    #[test]
    fn clean_dirty_and_busy_states_never_save_even_signed_in() {
        assert!(!save_gate(&signed_in(), None, false, false).0, "clean draft");
        assert!(!save_gate(&signed_in(), None, true, true).0, "already busy");
        assert!(
            !save_gate(&signed_in(), Some("name is required".into()), true, false).0,
            "invalid spec"
        );
    }

    #[test]
    fn spec_problem_surfaces_once_signed_in() {
        let (_, problem) = save_gate(&signed_in(), Some("name is required".into()), true, false);
        assert_eq!(problem.as_deref(), Some("name is required"));
    }
}

//! Drives the browser half of the OIDC login: reads `/api/auth/config`,
//! runs the PKCE redirect, and completes the token exchange.
//!
//! The state machine is pure and unit-tested; the effectful edges (network,
//! `location`, storage) are injected so the transitions can be verified
//! without a browser.

use crate::auth::{self, AuthConfig, Pkce};

/// Where the frontend is in the sign-in flow.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Phase {
    /// Checking `/api/auth/config`.
    Loading,
    /// Auth disabled on the backend (local dev): everything is open.
    Disabled,
    /// No usable token; `SignIn` carries the authorize URL to navigate to.
    SignedOut { sign_in: Option<String> },
    /// A fresh bearer token is available.
    SignedIn,
    /// A stored token was rejected or expired mid-session.
    Expired { message: String, sign_in: Option<String> },
    /// The IdP or the exchange failed.
    Failed { message: String, sign_in: Option<String> },
}

impl Phase {
    /// Whether writes can proceed with a token.
    pub(crate) fn can_write(&self) -> bool {
        matches!(self, Phase::SignedIn | Phase::Disabled)
    }

    /// One line for the top bar: what the user is, and what to do next.
    pub(crate) fn notice(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Phase::Disabled => Some(("auth off", "backend auth is disabled; writes are open")),
            Phase::Expired { .. } => Some(("session expired", "sign in again to save reports")),
            Phase::Failed { .. } => Some(("sign-in failed", "see the message; try again")),
            _ => None,
        }
    }
}

/// The sign-in URL to offer, if the flow can be started.
pub(crate) fn sign_in_url(phase: &Phase) -> Option<String> {
    match phase {
        Phase::SignedOut { sign_in } | Phase::Expired { sign_in, .. } | Phase::Failed { sign_in, .. } => sign_in.clone(),
        _ => None,
    }
}

/// Start a sign-in: mint PKCE + state, persist them, and return the URL to
/// navigate to. `None` when the backend has no usable OIDC config.
pub(crate) fn begin(config: &AuthConfig, redirect_uri: &str) -> Option<String> {
    let pkce: Pkce = auth::new_pkce().ok()?;
    let state = auth::new_state().ok()?;
    let url = auth::authorize_url(config, redirect_uri, &pkce, &state)?;
    auth::save_pending(&pkce, &state);
    Some(url)
}

/// Validate the callback's `state` against what was stored when the flow
/// started. A mismatch means the callback did not come from our own request
/// (or came from a stale tab), so the pending verifier is discarded.
pub(crate) fn check_state(returned: &str) -> Result<(), String> {
    match auth::take_pending() {
        Some((_, expected)) if expected == returned => Ok(()),
        Some(_) => Err("sign-in state did not match; the request may have been forged".into()),
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

    #[test]
    fn disabled_phase_can_write_because_backend_is_open() {
        assert!(Phase::Disabled.can_write());
        assert_eq!(
            Phase::Disabled.notice(),
            Some((
                "auth off",
                "backend auth is disabled; writes are open"
            ))
        );
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
        assert!(Phase::SignedIn.can_write());
        assert_eq!(Phase::SignedIn.notice(), None);
        assert_eq!(sign_in_url(&Phase::SignedIn), None);
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
        let (ok, problem) = save_gate(&Phase::SignedIn, None, true, false);
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
        assert!(!save_gate(&Phase::SignedIn, None, false, false).0, "clean draft");
        assert!(!save_gate(&Phase::SignedIn, None, true, true).0, "already busy");
        assert!(
            !save_gate(&Phase::SignedIn, Some("name is required".into()), true, false).0,
            "invalid spec"
        );
    }

    #[test]
    fn spec_problem_surfaces_once_signed_in() {
        let (_, problem) = save_gate(&Phase::SignedIn, Some("name is required".into()), true, false);
        assert_eq!(problem.as_deref(), Some("name is required"));
    }
}

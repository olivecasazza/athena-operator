//! Authenticated HTTP for the console frontend.
//!
//! Every mutating call goes through here so the bearer token is attached in
//! exactly one place and a 401/403 becomes an actionable message instead of a
//! raw body dump. Read-only GETs stay plain, matching the backend's open read
//! surface.

use crate::auth;
use serde::de::DeserializeOwned;

/// Failure of an API call, with the message the UI shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ApiError {
    /// No usable session. `reauth` says the user can sign in again.
    Unauthorized { message: String, reauth: bool },
    /// Authenticated, but the account lacks the required role.
    Forbidden(String),
    /// Any other non-success status.
    Status { code: u16, body: String },
    /// Transport or decode failure.
    Transport(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized { message, reauth } => {
                write!(f, "{message}")?;
                if *reauth {
                    write!(f, " (sign in again to save)")
                } else {
                    Ok(())
                }
            }
            ApiError::Forbidden(m) => write!(f, "{m}"),
            ApiError::Status { code, body } => write!(f, "HTTP {code}: {body}"),
            ApiError::Transport(m) => write!(f, "{m}"),
        }
    }
}

/// Build a request with the bearer token attached when a fresh one exists.
///
/// A token that is absent or inside the expiry skew yields a request with no
/// `Authorization` header, so the backend answers 401 and the caller can
/// offer sign-in — rather than sending a token guaranteed to be rejected.
pub(crate) fn with_bearer(builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match auth::bearer_token(auth::now_seconds()) {
        Some(token) => builder.bearer_auth(token),
        None => builder,
    }
}

/// Turn a non-success response into an [`ApiError`], preserving the backend's
/// own message where it sent one.
pub(crate) fn error_for(status: u16, body: &str) -> ApiError {
    let body = body.trim();
    match status {
        401 => ApiError::Unauthorized {
            message: if body.is_empty() {
                "not signed in".to_string()
            } else {
                body.to_string()
            },
            reauth: true,
        },
        403 => ApiError::Forbidden(if body.is_empty() {
            "your account lacks the admin role needed for this write".to_string()
        } else {
            body.to_string()
        }),
        _ => ApiError::Status { code: status, body: body.to_string() },
    }
}

/// Send a request, attach the token, and decode a JSON success body.
pub(crate) async fn send_json<T: DeserializeOwned>(
    builder: reqwest::RequestBuilder,
) -> Result<T, ApiError> {
    let resp = with_bearer(builder)
        .send()
        .await
        .map_err(|e| ApiError::Transport(e.to_string()))?;
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        if status == 401 {
            auth::mark_rejected();
        }
        return Err(error_for(status, &body));
    }
    serde_json::from_str(&body).map_err(|e| ApiError::Transport(format!("malformed response: {e}")))
}

/// Send a request and return a text success body.
pub(crate) async fn send_text(
    builder: reqwest::RequestBuilder,
) -> Result<String, ApiError> {
    let resp = with_bearer(builder)
        .send()
        .await
        .map_err(|e| ApiError::Transport(e.to_string()))?;
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        if status == 401 {
            auth::mark_rejected();
        }
        return Err(error_for(status, &body));
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthorized_carries_a_recovery_hint() {
        let err = error_for(401, "");
        assert_eq!(
            err,
            ApiError::Unauthorized { message: "not signed in".into(), reauth: true }
        );
        assert!(err.to_string().contains("sign in again"));
    }

    #[test]
    fn unauthorized_keeps_the_server_message() {
        let err = error_for(401, "invalid or insufficient token");
        assert_eq!(
            err,
            ApiError::Unauthorized {
                message: "invalid or insufficient token".into(),
                reauth: true
            }
        );
    }

    #[test]
    fn forbidden_names_the_missing_role_and_does_not_prompt_sign_in() {
        let err = error_for(403, "");
        assert!(matches!(err, ApiError::Forbidden(_)));
        let text = err.to_string();
        assert!(text.contains("admin"), "unhelpful 403 copy: {text}");
        assert!(!text.contains("sign in"), "403 is not a sign-in problem: {text}");
    }

    #[test]
    fn other_statuses_are_reported_verbatim() {
        let err = error_for(409, "resourceVersion conflict");
        assert_eq!(
            err,
            ApiError::Status { code: 409, body: "resourceVersion conflict".into() }
        );
        assert!(err.to_string().contains("409"));
    }

    #[test]
    fn body_whitespace_is_trimmed_before_use() {
        let err = error_for(401, "  \n ");
        assert_eq!(
            err,
            ApiError::Unauthorized { message: "not signed in".into(), reauth: true }
        );
    }
}

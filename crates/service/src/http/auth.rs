//! Operator auth: static user list from `meshmon.toml`, session cookies
//! via `tower-sessions`, and per-IP rate limiting on the login endpoint.
//!
//! Session cookies use `Secure` + `HttpOnly` + `SameSite=Lax` with a 30-day
//! rolling expiry. Logins go through `AuthSession::login()` from
//! `axum-login`; the `session_auth_hash` hashes the stored PHC string, so a
//! password-hash change in the config invalidates existing sessions for that
//! user at next request (though the spec notes full `[auth]` changes warrant
//! a restart anyway).

use crate::config::Config;
use arc_swap::ArcSwap;
use axum_login::{AuthUser as AxumAuthUser, AuthnBackend, UserId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

/// Principal returned by the backend on successful authentication. Stored in
/// the session by `axum-login`; retrieved via the `AuthSession` extractor.
#[derive(Debug, Clone)]
pub struct Principal {
    /// Username from `[auth.users].username` in `meshmon.toml`.
    pub username: String,
    /// PHC-formatted argon2 hash. Captured at authenticate time so we can
    /// compute `session_auth_hash` without re-reading the config snapshot.
    pub password_hash: String,
}

impl AxumAuthUser for Principal {
    type Id = String;

    fn id(&self) -> Self::Id {
        self.username.clone()
    }

    fn session_auth_hash(&self) -> &[u8] {
        self.password_hash.as_bytes()
    }
}

/// POST body for `/api/auth/login`.
#[derive(Deserialize, ToSchema)]
pub struct LoginRequest {
    /// Username from the configured `[auth.users]` list.
    pub username: String,
    /// Plaintext password. Verified against the PHC hash via argon2 inside
    /// `spawn_blocking`.
    pub password: String,
}

impl std::fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginRequest")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// JSON response body for `/api/auth/login`.
#[derive(Debug, Serialize, ToSchema)]
pub struct LoginResponse {
    /// Echoed username on success.
    pub username: String,
}

/// `AuthnBackend` implementation. Holds an `Arc<ArcSwap<Config>>` so config
/// reloads are picked up for the next authentication attempt (existing
/// sessions are unaffected — a full restart is still required for
/// `[auth]` changes per spec 03).
#[derive(Clone)]
pub struct ConfigAuthBackend {
    config: Arc<ArcSwap<Config>>,
}

impl ConfigAuthBackend {
    /// Construct the backend from the service's shared `Config` handle.
    pub fn new(config: Arc<ArcSwap<Config>>) -> Self {
        Self { config }
    }
}

/// `AuthnBackend` error. Authentication failures due to wrong credentials
/// return `Ok(None)`, not an error — only infrastructure faults raise
/// `AuthError`.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// `argon2` verification task panicked or was cancelled.
    #[error("password verification task failed: {0}")]
    VerifyTask(#[from] tokio::task::JoinError),
}

impl AuthnBackend for ConfigAuthBackend {
    type User = Principal;
    type Credentials = LoginRequest;
    type Error = AuthError;

    async fn authenticate(
        &self,
        LoginRequest { username, password }: Self::Credentials,
    ) -> Result<Option<Self::User>, Self::Error> {
        // Snapshot the config so a concurrent reload can't tear the slice.
        let cfg = self.config.load_full();
        // Look up the user by exact username (case-sensitive — operators
        // know their handles).
        let Some(user) = cfg
            .auth
            .users
            .iter()
            .find(|u| u.username == username)
            .cloned()
        else {
            // Run a dummy verification anyway to keep the response timing
            // roughly flat and avoid username enumeration via latency.
            dummy_verify(password).await?;
            return Ok(None);
        };
        let hash = user.password_hash.clone();
        let matched =
            tokio::task::spawn_blocking(move || verify_password(&password, &hash)).await?;
        if matched {
            Ok(Some(Principal {
                username: user.username,
                password_hash: user.password_hash,
            }))
        } else {
            Ok(None)
        }
    }

    async fn get_user(&self, user_id: &UserId<Self>) -> Result<Option<Self::User>, Self::Error> {
        let cfg = self.config.load_full();
        Ok(cfg
            .auth
            .users
            .iter()
            .find(|u| &u.username == user_id)
            .map(|u| Principal {
                username: u.username.clone(),
                password_hash: u.password_hash.clone(),
            }))
    }
}

/// Blocking password verify. Uses argon2's internal `password-hash` 0.5
/// re-export (the workspace has `password-hash` 0.6 for config-parse
/// validation, but `argon2::PasswordVerifier::verify_password` takes a
/// 0.5 `PasswordHash`).
fn verify_password(plaintext: &str, phc: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    use argon2::Argon2;
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(plaintext.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => {
            // Config validation already rejected malformed PHC at load.
            // If this ever fires, the runtime config diverged from the
            // validator — treat as non-match rather than raising.
            false
        }
    }
}

/// Run a throwaway verify against a fixed hash to keep response timing
/// similar between "user not found" and "wrong password". Panics silently
/// on failure — this is defence-in-depth, not a correctness gate.
async fn dummy_verify(password: String) -> Result<(), AuthError> {
    const DUMMY_HASH: &str =
        "$argon2id$v=19$m=16,t=1,p=1$c2FsdHNhbHQ$87ARSxtFrFp/0EGLYgzI7Giyu6y7PD1rUqoZugn3NqY";
    tokio::task::spawn_blocking(move || {
        let _ = verify_password(&password, DUMMY_HASH);
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    /// PHC hash of the password `"correct horse battery staple"` generated
    /// with the weakest argon2 parameters (`m=16,t=1,p=1`) so tests stay
    /// fast. Regenerate with a throwaway `argon2` call using
    /// `Params::new(16, 1, 1, None)` if the password ever changes.
    const TEST_HASH: &str =
        "$argon2id$v=19$m=16,t=1,p=1$c2FsdHNhbHQ$87ARSxtFrFp/0EGLYgzI7Giyu6y7PD1rUqoZugn3NqY";

    fn cfg_with_user(username: &str, hash: &str) -> Arc<ArcSwap<Config>> {
        let toml = format!(
            r#"
[database]
url = "postgres://ignored@h/d"

[[auth.users]]
username = "{username}"
password_hash = "{hash}"
"#
        );
        let cfg = Arc::new(Config::from_str(&toml, "test.toml").expect("parse"));
        Arc::new(ArcSwap::from(cfg))
    }

    #[tokio::test]
    async fn authenticate_returns_user_on_correct_password() {
        let cfg = cfg_with_user("alice", TEST_HASH);
        let backend = ConfigAuthBackend::new(cfg);
        let creds = LoginRequest {
            username: "alice".into(),
            password: "correct horse battery staple".into(),
        };
        let user = backend.authenticate(creds).await.expect("no infra error");
        assert!(user.is_some());
        assert_eq!(user.unwrap().username, "alice");
    }

    #[tokio::test]
    async fn authenticate_returns_none_on_wrong_password() {
        let cfg = cfg_with_user("alice", TEST_HASH);
        let backend = ConfigAuthBackend::new(cfg);
        let creds = LoginRequest {
            username: "alice".into(),
            password: "wrong".into(),
        };
        let user = backend.authenticate(creds).await.expect("no infra error");
        assert!(user.is_none());
    }

    #[tokio::test]
    async fn authenticate_returns_none_on_unknown_user() {
        let cfg = cfg_with_user("alice", TEST_HASH);
        let backend = ConfigAuthBackend::new(cfg);
        let creds = LoginRequest {
            username: "eve".into(),
            password: "correct horse battery staple".into(),
        };
        let user = backend.authenticate(creds).await.expect("no infra error");
        assert!(user.is_none());
    }

    #[test]
    fn login_request_debug_redacts_password() {
        let req = LoginRequest {
            username: "alice".into(),
            password: "hunter2".into(),
        };
        let rendered = format!("{req:?}");
        assert!(rendered.contains("alice"), "rendered = {rendered}");
        assert!(!rendered.contains("hunter2"), "rendered = {rendered}");
        assert!(rendered.contains("<redacted>"), "rendered = {rendered}");
    }
}

//! Operator auth: static user list from `meshmon.toml`, session cookies
//! via `tower-sessions`, and per-IP rate limiting on the login endpoint.
//!
//! Session cookies use `Secure` + `HttpOnly` + `SameSite=Lax` with a 30-day
//! rolling expiry. Logins go through `AuthSession::login()` from `axum-login`;
//! `session_auth_hash` returns the stored PHC string as an opaque
//! identity token (stored verbatim in the session store, compared
//! with constant-time equality), so a password-hash change in the
//! config invalidates existing sessions for that user at next
//! request (though the spec notes full `[auth]` changes warrant
//! a restart anyway).

use crate::config::Config;
use arc_swap::ArcSwap;
use axum::http::request::Parts;
use axum_login::{AuthUser as AxumAuthUser, AuthnBackend, UserId};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tower_governor::errors::GovernorError;
use tower_governor::key_extractor::KeyExtractor;
use utoipa::ToSchema;

/// Extract the leftmost client IP from an `X-Forwarded-For` value.
///
/// Accepts the de-facto syntax `client, proxy1, proxy2`. Returns the
/// first parseable `IpAddr`; returns `None` if the header is malformed
/// or contains no parseable address.
pub(crate) fn parse_xff_client_ip(header: &str) -> Option<std::net::IpAddr> {
    header
        .split(',')
        .next()
        .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok())
}

/// Extract the leftmost `for=<ip>` from an RFC 7239 `Forwarded` value.
///
/// Handles the common real-world shapes: `for=1.2.3.4`,
/// `for="1.2.3.4"`, `for="1.2.3.4:5678"`, `for="[::1]:4711"`,
/// `for=[2001:db8::1]`. Port is stripped. Returns `None` when no
/// parseable `for=` parameter is found.
pub(crate) fn parse_forwarded_client_ip(header: &str) -> Option<std::net::IpAddr> {
    let first_element = header.split(',').next()?;
    for pair in first_element.split(';') {
        // Skip malformed pairs (no `=`) rather than bailing out of the
        // whole parse — a leading `;` or a stray token before `for=`
        // should not make the parser forget what comes after.
        let Some((key, value)) = pair.trim().split_once('=') else {
            continue;
        };
        if !key.trim().eq_ignore_ascii_case("for") {
            continue;
        }
        let value = value.trim().trim_matches('"');
        // Bracketed IPv6: [addr] or [addr]:port.
        let stripped = if let Some(rest) = value.strip_prefix('[') {
            rest.split(']').next().unwrap_or(rest)
        } else if let Some((host, maybe_port)) = value.rsplit_once(':') {
            // IPv4 with port (`host:port`) — IPv6 is only valid with
            // brackets per RFC 7239 §6. `maybe_port` must parse as u16
            // to distinguish `host:port` from bare `::1`.
            if maybe_port.chars().all(|c| c.is_ascii_digit()) {
                host
            } else {
                value
            }
        } else {
            value
        };
        if let Ok(ip) = stripped.parse::<std::net::IpAddr>() {
            return Some(ip);
        }
    }
    None
}

/// Client-IP extraction shared between the login rate limit, the agent
/// rate limit, and the tonic register handler's IP identity check.
///
/// When `trust_forwarded = true`, try the leftmost entry of
/// `X-Forwarded-For` first; if missing or malformed, try RFC 7239
/// `Forwarded: for=...`. On any fallback, use the `ConnectInfo` peer.
/// When `trust_forwarded = false`, only `ConnectInfo` is read.
#[allow(dead_code)]
pub(crate) fn client_ip(parts: &Parts, trust_forwarded: bool) -> Option<std::net::IpAddr> {
    if trust_forwarded {
        if let Some(ip) = parts
            .headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_xff_client_ip)
        {
            return Some(ip);
        }
        if let Some(ip) = parts
            .headers
            .get("forwarded")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_forwarded_client_ip)
        {
            return Some(ip);
        }
    }
    parts
        .extensions
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip())
}

/// Constant-time equality over byte slices of *equal* length. `ct_eq` on
/// `[u8]` already returns `Choice(0)` for unequal lengths (subtle 2.x
/// short-circuits — no panic), but we reject unequal lengths up front to
/// keep the fast path simple and make the length-leak explicit. Leaks
/// "token-not-empty-but-wrong-length" by design — cheap to discover by
/// trial-and-error anyway.
pub(crate) fn constant_time_eq_bytes(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    bool::from(a.ct_eq(b))
}

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

    /// axum-login stores these bytes verbatim in the session store
    /// (`Vec<u8>`, constant-time compared on each request). With
    /// `MemoryStore` the PHC string stays in-process; swapping to a
    /// persistent store (Sqlite/Redis/etc.) would persist PHC hashes in
    /// that store — revisit this if the session backend changes.
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
            dummy_verify(password).await;
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

/// Run a throwaway verify against a fixed hash to partially flatten
/// response timing between "user not found" and "wrong password".
///
/// Caveat: argon2's `verify_password` executes with the parameters
/// embedded in the parsed PHC string, not with `Argon2::default()`'s
/// params. So the dummy only matches real-user timing when the operator's
/// hashes share roughly these parameters. Weak production hashes (e.g.
/// default `argon2` CLI output) will run much longer than this dummy, and
/// the latency gap is still observable. Treat this as best-effort
/// defence-in-depth, not a full mitigation.
///
/// `spawn_blocking` errors are logged and swallowed — propagating them
/// would let an attacker distinguish known from unknown users via 500 vs
/// 401 responses on panics.
async fn dummy_verify(password: String) {
    const DUMMY_HASH: &str =
        "$argon2id$v=19$m=16,t=1,p=1$c2FsdHNhbHQ$87ARSxtFrFp/0EGLYgzI7Giyu6y7PD1rUqoZugn3NqY";
    if let Err(e) = tokio::task::spawn_blocking(move || {
        let _ = verify_password(&password, DUMMY_HASH);
    })
    .await
    {
        tracing::warn!(error = %e, "dummy password verify task failed");
    }
}

/// Type alias for the concrete `AuthSession` wired through the service.
pub type AuthSession = axum_login::AuthSession<ConfigAuthBackend>;

/// POST `/api/auth/login` — authenticate and issue a session cookie.
///
/// - 200 + `LoginResponse` on success
/// - 401 on wrong credentials (JSON body: `{"error": "invalid credentials"}`)
/// - 500 only on infra failure (session store I/O, verify-task panic)
#[utoipa::path(
    post,
    path = "/api/auth/login",
    tag = "auth",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Logged in", body = LoginResponse),
        (status = 401, description = "Invalid credentials"),
        (status = 429, description = "Too many attempts from this IP"),
    )
)]
#[tracing::instrument(skip_all, fields(username = %creds.username))]
pub async fn login(
    mut auth_session: AuthSession,
    axum::Json(creds): axum::Json<LoginRequest>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    let user = match auth_session.authenticate(creds).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            tracing::warn!("login failed: invalid credentials");
            return (
                StatusCode::UNAUTHORIZED,
                axum::Json(serde_json::json!({"error": "invalid credentials"})),
            )
                .into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, "auth backend infra failure");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": "internal"})),
            )
                .into_response();
        }
    };

    if let Err(e) = auth_session.login(&user).await {
        tracing::error!(error = %e, "session login failed");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({"error": "internal"})),
        )
            .into_response();
    }

    axum::Json(LoginResponse {
        username: user.username,
    })
    .into_response()
}

/// POST `/api/auth/logout` — invalidate the current session.
///
/// Idempotent: 200 whether or not the caller was logged in. The cookie is
/// cleared either way.
#[utoipa::path(
    post,
    path = "/api/auth/logout",
    tag = "auth",
    responses(
        (status = 200, description = "Logged out"),
    )
)]
#[tracing::instrument(skip_all)]
pub async fn logout(mut auth_session: AuthSession) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    if let Err(e) = auth_session.logout().await {
        tracing::warn!(error = %e, "session logout failed — cookie still cleared client-side");
    }
    StatusCode::OK.into_response()
}

/// Build the tower-sessions middleware stack: in-memory store, cookie name
/// `meshmon_session`, 30-day rolling expiry, `Secure`+`HttpOnly`+`SameSite=Lax`.
///
/// Returns the store handle alongside the layer so callers can keep a
/// reference for future migrations to a persistent store. Note:
/// `MemoryStore` does not implement `ExpiredDeletion` — expired sessions
/// are filtered on read but never purged from the in-memory map, so memory
/// usage grows until process restart. Expected operator counts are small,
/// so this is acceptable; revisit if switching to a persistent session
/// store.
pub fn session_layer() -> (
    tower_sessions::SessionManagerLayer<tower_sessions::MemoryStore>,
    tower_sessions::MemoryStore,
) {
    use tower_sessions::cookie::time::Duration;
    use tower_sessions::cookie::SameSite;
    use tower_sessions::{Expiry, MemoryStore, SessionManagerLayer};

    let store = MemoryStore::default();
    let layer = SessionManagerLayer::new(store.clone())
        .with_name("meshmon_session")
        .with_secure(true)
        .with_http_only(true)
        .with_same_site(SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(Duration::days(30)));
    (layer, store)
}

/// `axum-login`'s auth layer bound to our `ConfigAuthBackend`.
pub fn auth_manager_layer(
    backend: ConfigAuthBackend,
    session_layer: tower_sessions::SessionManagerLayer<tower_sessions::MemoryStore>,
) -> axum_login::AuthManagerLayer<ConfigAuthBackend, tower_sessions::MemoryStore> {
    axum_login::AuthManagerLayerBuilder::new(backend, session_layer).build()
}

/// Spec-pinned login rate: 5 attempts per 15 minutes with a burst of 3.
pub const LOGIN_BURST_SIZE: u32 = 3;
/// One permitted attempt every 180s (5 / 15 min).
pub const LOGIN_SECONDS_PER_REQUEST: u64 = 180;

/// `KeyExtractor` that reads the IP from `ConnectInfo<SocketAddr>` only,
/// ignoring forwarded headers. Used when `service.trust_forwarded_headers`
/// is `false` so attackers can't forge `X-Forwarded-For` to bypass the
/// per-IP login rate limit.
///
/// Requires the app to be served via
/// `axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>())`.
/// Without the `ConnectInfo` extension, this extractor returns
/// `GovernorError::UnableToExtractKey`, which tower_governor translates to
/// HTTP 500 — meaning every login would fail silently.
#[derive(Clone)]
pub struct PeerAddrKeyExtractor;

impl KeyExtractor for PeerAddrKeyExtractor {
    type Key = IpAddr;

    fn extract<T>(&self, req: &axum::http::Request<T>) -> Result<Self::Key, GovernorError> {
        req.extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|ci| ci.0.ip())
            .ok_or(GovernorError::UnableToExtractKey)
    }
}

/// Two concrete shapes for the login rate-limit layer — one per
/// `trust_forwarded_headers` setting. Both are wrapped in `tower::util::Either`
/// so callers see a single `Layer` type regardless of the branch.
///
/// `RespBody` is pinned to `axum::body::Body` because
/// `Response<axum::body::Body>: From<GovernorError>` is provided by
/// tower_governor's `axum` feature (which is in the crate's default feature
/// set).
pub type LoginRateLimitLayer = tower::util::Either<
    tower_governor::GovernorLayer<
        tower_governor::key_extractor::SmartIpKeyExtractor,
        governor::middleware::NoOpMiddleware,
        axum::body::Body,
    >,
    tower_governor::GovernorLayer<
        PeerAddrKeyExtractor,
        governor::middleware::NoOpMiddleware,
        axum::body::Body,
    >,
>;

/// Build the login rate-limit layer using the spec parameters and the
/// configured trust mode.
pub fn login_rate_limit_layer(trust_forwarded_headers: bool) -> LoginRateLimitLayer {
    use tower_governor::governor::GovernorConfigBuilder;
    use tower_governor::key_extractor::SmartIpKeyExtractor;
    use tower_governor::GovernorLayer;

    if trust_forwarded_headers {
        let cfg = GovernorConfigBuilder::default()
            .per_second(LOGIN_SECONDS_PER_REQUEST)
            .burst_size(LOGIN_BURST_SIZE)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .expect("governor config (smart)");
        tower::util::Either::Left(GovernorLayer::new(cfg))
    } else {
        let cfg = GovernorConfigBuilder::default()
            .per_second(LOGIN_SECONDS_PER_REQUEST)
            .burst_size(LOGIN_BURST_SIZE)
            .key_extractor(PeerAddrKeyExtractor)
            .finish()
            .expect("governor config (peer-only)");
        tower::util::Either::Right(GovernorLayer::new(cfg))
    }
}

use crate::state::AppState;
use tonic::{Request as TonicRequest, Status};

/// Bearer-token gate for the tonic `AgentApi` service (design §3.4).
///
/// Returns a cloneable closure so `AgentApiServer::with_interceptor` can
/// accept it. The closure reads `state.config()` on every call so SIGHUP
/// rotations take effect on the next RPC without rebuilding the server.
pub fn agent_grpc_interceptor(
    state: AppState,
) -> impl Fn(TonicRequest<()>) -> Result<TonicRequest<()>, Status> + Clone {
    move |req: TonicRequest<()>| -> Result<TonicRequest<()>, Status> {
        let cfg = state.config();
        let Some(expected) = cfg.agent_api.shared_token.as_deref() else {
            return Err(Status::unavailable("agent api not configured"));
        };
        let Some(header) = req.metadata().get("authorization") else {
            return Err(Status::unauthenticated("missing bearer token"));
        };
        let Ok(raw) = header.to_str() else {
            return Err(Status::unauthenticated("malformed authorization metadata"));
        };
        // HTTP auth schemes are case-insensitive (RFC 7235 §2.1). Accept
        // "Bearer", "bearer", "BEARER", etc. before the single required
        // space — clients that normalize scheme case should not be
        // rejected as unauthenticated.
        let Some(presented) = raw
            .get(..7)
            .filter(|prefix| prefix.eq_ignore_ascii_case("Bearer "))
            .and_then(|_| raw.get(7..))
        else {
            return Err(Status::unauthenticated("missing bearer prefix"));
        };
        if !constant_time_eq_bytes(presented.as_bytes(), expected.as_bytes()) {
            return Err(Status::unauthenticated("invalid bearer token"));
        }
        Ok(req)
    }
}

/// Agent-API rate-limit layer (same shape as the login layer). Applied via
/// `tonic::transport::Server::builder().layer(...)` for the gRPC half.
pub type AgentApiRateLimitLayer = tower::util::Either<
    tower_governor::GovernorLayer<
        tower_governor::key_extractor::SmartIpKeyExtractor,
        governor::middleware::NoOpMiddleware,
        axum::body::Body,
    >,
    tower_governor::GovernorLayer<
        PeerAddrKeyExtractor,
        governor::middleware::NoOpMiddleware,
        axum::body::Body,
    >,
>;

/// Replenish interval for the agent-API rate limiter, in nanoseconds.
///
/// Nanosecond precision avoids the seconds-only quantization bug where e.g.
/// `per_minute = 59` rounded up to `per_second(2)` enforces 30 req/min, or
/// `per_minute = 120` rounded down to `per_second(1)` enforces 60 req/min.
/// Config parsing guarantees `per_minute > 0`; the `max(1)` calls are
/// defense in depth so a future refactor does not produce a panic here.
fn rate_limit_period_nanos(per_minute: u32) -> u64 {
    60_000_000_000u64
        .checked_div(u64::from(per_minute.max(1)))
        .unwrap_or(1_000_000_000)
        .max(1)
}

/// Build the rate-limit layer from resolved `[agent_api]` knobs + trust mode.
/// Non-zero `per_minute` and `burst` are enforced at config parse time.
pub fn agent_api_rate_limit_layer(
    trust_forwarded: bool,
    per_minute: u32,
    burst: u32,
) -> AgentApiRateLimitLayer {
    use tower_governor::governor::GovernorConfigBuilder;
    use tower_governor::key_extractor::SmartIpKeyExtractor;
    use tower_governor::GovernorLayer;

    let period_nanos = rate_limit_period_nanos(per_minute);

    if trust_forwarded {
        let cfg = GovernorConfigBuilder::default()
            .per_nanosecond(period_nanos)
            .burst_size(burst)
            .key_extractor(SmartIpKeyExtractor)
            .finish()
            .expect("governor config (smart)");
        tower::util::Either::Left(GovernorLayer::new(cfg))
    } else {
        let cfg = GovernorConfigBuilder::default()
            .per_nanosecond(period_nanos)
            .burst_size(burst)
            .key_extractor(PeerAddrKeyExtractor)
            .finish()
            .expect("governor config (peer-only)");
        tower::util::Either::Right(GovernorLayer::new(cfg))
    }
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

[probing]
udp_probe_secret = "hex:6d73686d6e2d7631"
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

    #[tokio::test]
    async fn get_user_returns_user_when_present() {
        let cfg = cfg_with_user("alice", TEST_HASH);
        let backend = ConfigAuthBackend::new(cfg);
        let user = backend
            .get_user(&"alice".to_string())
            .await
            .expect("no infra error");
        assert!(user.is_some());
        assert_eq!(user.unwrap().username, "alice");
    }

    #[tokio::test]
    async fn get_user_returns_none_when_absent() {
        let cfg = cfg_with_user("alice", TEST_HASH);
        let backend = ConfigAuthBackend::new(cfg);
        let user = backend
            .get_user(&"bob".to_string())
            .await
            .expect("no infra error");
        assert!(user.is_none());
    }

    #[tokio::test]
    async fn get_user_reflects_config_reload() {
        let cfg = cfg_with_user("alice", TEST_HASH);
        let backend = ConfigAuthBackend::new(cfg.clone());
        assert!(backend
            .get_user(&"alice".to_string())
            .await
            .expect("no infra")
            .is_some());
        // Simulate a config reload that removes alice.
        let new_toml = r#"
[database]
url = "postgres://ignored@h/d"

[probing]
udp_probe_secret = "hex:6d73686d6e2d7631"
"#;
        let new_cfg = Arc::new(Config::from_str(new_toml, "test.toml").expect("parse"));
        cfg.store(new_cfg);
        assert!(backend
            .get_user(&"alice".to_string())
            .await
            .expect("no infra")
            .is_none());
    }

    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn parts_with_xff_and_peer(xff: Option<&str>, peer: IpAddr) -> axum::http::request::Parts {
        let mut req = Request::builder().uri("/");
        if let Some(v) = xff {
            req = req.header("x-forwarded-for", v);
        }
        let (mut parts, _) = req.body(()).unwrap().into_parts();
        parts
            .extensions
            .insert(ConnectInfo(SocketAddr::new(peer, 9999)));
        parts
    }

    #[test]
    fn client_ip_prefers_leftmost_xff_when_trusted() {
        let parts = parts_with_xff_and_peer(
            Some("198.51.100.7, 203.0.113.1"),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        );
        assert_eq!(
            client_ip(&parts, true).unwrap(),
            IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7))
        );
    }

    #[test]
    fn client_ip_ignores_xff_when_not_trusted() {
        let parts =
            parts_with_xff_and_peer(Some("198.51.100.7"), IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
        assert_eq!(
            client_ip(&parts, false).unwrap(),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))
        );
    }

    #[test]
    fn client_ip_falls_back_to_peer_on_missing_xff() {
        let parts = parts_with_xff_and_peer(None, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)));
        assert_eq!(
            client_ip(&parts, true).unwrap(),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2))
        );
    }

    #[test]
    fn client_ip_falls_back_to_peer_on_malformed_xff() {
        let parts =
            parts_with_xff_and_peer(Some("not-an-ip"), IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3)));
        assert_eq!(
            client_ip(&parts, true).unwrap(),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 3))
        );
    }

    #[test]
    fn constant_time_eq_bytes_true_on_match() {
        assert!(constant_time_eq_bytes(b"hello", b"hello"));
    }

    #[test]
    fn constant_time_eq_bytes_false_on_mismatch() {
        assert!(!constant_time_eq_bytes(b"hello", b"world"));
    }

    #[test]
    fn constant_time_eq_bytes_false_on_length_mismatch() {
        assert!(!constant_time_eq_bytes(b"abc", b"abcd"));
    }

    // ----- interceptor tests -----

    use crate::ingestion::{IngestionConfig, IngestionPipeline};
    use crate::registry::AgentRegistry;
    use crate::state::AppState;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    /// Build an `AppState` with the given optional agent token wired into
    /// `[agent_api].shared_token`. The pool is lazy (no real DB connection),
    /// the ingestion pipeline is spawned against a token that's immediately
    /// cancelled so no background threads linger between tests.
    ///
    /// Sync on purpose: unit tests in this module use the plain `#[test]`
    /// harness, and `crate::metrics::test_install()` mirrors that with a
    /// sync `OnceLock`. Integration tests have their own async
    /// `test_prometheus_handle()` helper in `tests/common/mod.rs` —
    /// don't "normalize" this one to async or you'll force every
    /// surrounding `#[test]` to become `#[tokio::test]`.
    fn agent_state_with(token: Option<&str>) -> AppState {
        let token_line = match token {
            Some(t) => format!(r#"shared_token = "{t}""#),
            None => String::new(),
        };
        let toml = format!(
            r#"
[database]
url = "postgres://ignored@localhost/db"

[agent_api]
{token_line}

[probing]
udp_probe_secret = "hex:6d73686d6e2d7631"
"#
        );
        let cfg = Arc::new(Config::from_str(&toml, "test.toml").expect("config parse"));
        let swap = Arc::new(ArcSwap::from(cfg.clone()));
        let (_, rx) = tokio::sync::watch::channel(cfg);

        let pool =
            sqlx::PgPool::connect_lazy("postgres://ignored@localhost/db").expect("lazy pool");
        let ct = CancellationToken::new();
        ct.cancel(); // cancel immediately so workers exit without doing DB work
        let ingestion = IngestionPipeline::spawn(
            IngestionConfig::default_with_url("http://vm-ignored:8428/api/v1/write".into()),
            pool.clone(),
            ct,
        );
        let registry = Arc::new(AgentRegistry::new(
            pool.clone(),
            Duration::from_secs(10),
            Duration::from_secs(300),
        ));
        AppState::new(
            swap,
            rx,
            pool,
            ingestion,
            registry,
            crate::metrics::test_install(),
        )
    }

    /// Call the interceptor with the given `Authorization` header value
    /// (pass `None` to omit the header entirely).
    fn run_interceptor(state: AppState, authz: Option<&str>) -> Result<TonicRequest<()>, Status> {
        let interceptor = agent_grpc_interceptor(state);
        let mut req = TonicRequest::new(());
        if let Some(v) = authz {
            req.metadata_mut()
                .insert("authorization", v.parse().expect("valid header value"));
        }
        interceptor(req)
    }

    #[tokio::test]
    async fn interceptor_unavailable_when_token_unset() {
        let state = agent_state_with(None);
        let err = run_interceptor(state, Some("Bearer anything")).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unavailable);
    }

    #[tokio::test]
    async fn interceptor_unauthenticated_without_header() {
        let state = agent_state_with(Some("secret-token"));
        let err = run_interceptor(state, None).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[tokio::test]
    async fn interceptor_unauthenticated_without_bearer_prefix() {
        let state = agent_state_with(Some("secret-token"));
        let err = run_interceptor(state, Some("secret-token")).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[tokio::test]
    async fn interceptor_unauthenticated_on_wrong_token_same_length() {
        let state = agent_state_with(Some("secret-token"));
        // Same length as "secret-token" but different content.
        let err = run_interceptor(state, Some("Bearer secret-XXXXX")).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[tokio::test]
    async fn interceptor_unauthenticated_on_wrong_length() {
        let state = agent_state_with(Some("secret-token"));
        let err = run_interceptor(state, Some("Bearer short")).unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unauthenticated);
    }

    #[tokio::test]
    async fn interceptor_ok_on_match() {
        let state = agent_state_with(Some("secret-token"));
        let result = run_interceptor(state, Some("Bearer secret-token"));
        assert!(result.is_ok());
    }

    #[test]
    fn agent_rate_limit_builds_for_both_trust_modes() {
        // Just verify construction doesn't panic — no HTTP traffic needed.
        let _ = agent_api_rate_limit_layer(true, 60, 30);
        let _ = agent_api_rate_limit_layer(false, 60, 30);
    }

    #[tokio::test]
    async fn interceptor_accepts_lowercase_bearer_scheme() {
        // HTTP auth schemes are case-insensitive (RFC 7235 §2.1); rejecting
        // "bearer" because we only checked for "Bearer " would be an
        // interoperability bug.
        let state = agent_state_with(Some("secret-token"));
        let result = run_interceptor(state, Some("bearer secret-token"));
        assert!(result.is_ok(), "interceptor must accept lowercase scheme");
    }

    #[tokio::test]
    async fn interceptor_rejects_bearer_without_separator() {
        // Require the single space after the scheme; "Bearertoken" should
        // not slice through `strip_prefix` behavior by accident.
        let state = agent_state_with(Some("secret-token"));
        let result = run_interceptor(state, Some("Bearersecret-token"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_xff_picks_leftmost_ip() {
        assert_eq!(
            parse_xff_client_ip("1.2.3.4, 10.0.0.1"),
            Some("1.2.3.4".parse().unwrap())
        );
        assert_eq!(
            parse_xff_client_ip(" 2001:db8::1 , fe80::1"),
            Some("2001:db8::1".parse().unwrap())
        );
        assert_eq!(parse_xff_client_ip("not-an-ip"), None);
        assert_eq!(parse_xff_client_ip(""), None);
    }

    #[test]
    fn parse_forwarded_handles_common_shapes() {
        // Bare IPv4.
        assert_eq!(
            parse_forwarded_client_ip("for=192.0.2.60;proto=http"),
            Some("192.0.2.60".parse().unwrap())
        );
        // Quoted IPv4.
        assert_eq!(
            parse_forwarded_client_ip(r#"for="192.0.2.60""#),
            Some("192.0.2.60".parse().unwrap())
        );
        // IPv4 with port.
        assert_eq!(
            parse_forwarded_client_ip(r#"for="192.0.2.60:47011""#),
            Some("192.0.2.60".parse().unwrap())
        );
        // Bracketed IPv6 with port.
        assert_eq!(
            parse_forwarded_client_ip(r#"for="[2001:db8::1]:4711""#),
            Some("2001:db8::1".parse().unwrap())
        );
        // Multiple forwarded-elements — take the leftmost (closest hop).
        assert_eq!(
            parse_forwarded_client_ip("for=192.0.2.60, for=10.0.0.1"),
            Some("192.0.2.60".parse().unwrap())
        );
        // Case-insensitive key.
        assert_eq!(
            parse_forwarded_client_ip("For=192.0.2.60"),
            Some("192.0.2.60".parse().unwrap())
        );
        // No `for=` → None.
        assert_eq!(parse_forwarded_client_ip("proto=http;by=10.0.0.1"), None);
        // Malformed → None.
        assert_eq!(parse_forwarded_client_ip("garbage"), None);
        // Malformed pair before a valid `for=` must not short-circuit the
        // search for subsequent pairs (defensive — real proxies don't
        // emit this shape).
        assert_eq!(
            parse_forwarded_client_ip(";for=192.0.2.60"),
            Some("192.0.2.60".parse().unwrap())
        );
        assert_eq!(
            parse_forwarded_client_ip("stray;for=192.0.2.60"),
            Some("192.0.2.60".parse().unwrap())
        );
    }

    #[test]
    fn rate_limit_period_nanos_matches_per_minute() {
        // 60/min = 1 req/sec.
        assert_eq!(rate_limit_period_nanos(60), 1_000_000_000);
        // 120/min = 500ms/req — previously truncated to `per_second(1)`
        // which enforced 60/min.
        assert_eq!(rate_limit_period_nanos(120), 500_000_000);
        // 59/min — previously rounded up to `per_second(2)` which
        // enforced 30/min.
        assert_eq!(rate_limit_period_nanos(59), 60_000_000_000 / 59);
        // Extreme low: 1/min = 60s/req.
        assert_eq!(rate_limit_period_nanos(1), 60_000_000_000);
        // Extreme high: 60_000/min = 1ms/req.
        assert_eq!(rate_limit_period_nanos(60_000), 1_000_000);
        // Pathological zero falls back to 1/min (defensive only — config
        // parsing rejects `per_minute = 0`).
        assert_eq!(rate_limit_period_nanos(0), 60_000_000_000);
    }
}

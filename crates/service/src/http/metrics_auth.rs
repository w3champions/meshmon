//! HTTP Basic auth guard for `/metrics`.
//!
//! Behaviour matches spec 03:
//! - Config unset (`[service.metrics_auth]` absent) — requests pass
//!   through unauthenticated.
//! - Config present — `Authorization: Basic <b64(user:pass)>` required.
//!   Username compared in constant time against the configured value;
//!   password verified against the PHC-formatted argon2 hash.
//! - Missing or invalid credentials yield `401` with
//!   `WWW-Authenticate: Basic realm="meshmon metrics"` so a Prometheus
//!   scraper (or curl) can probe and retry with credentials.
//!
//! Config is read from [`AppState`] on every request so SIGHUP reloads
//! take effect without rebuilding the router.

use crate::state::AppState;
use argon2::password_hash::{PasswordHash, PasswordVerifier};
use argon2::Argon2;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use subtle::ConstantTimeEq;

/// Axum middleware that enforces HTTP Basic auth on the request when
/// `[service.metrics_auth]` is configured. Passes through unauthenticated
/// requests when the config block is absent — the spec default.
pub async fn require_basic_auth(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let cfg = state.config();
    let Some(expected) = cfg.service.metrics_auth.as_ref() else {
        return next.run(req).await;
    };

    let Some(hdr) = req.headers().get(header::AUTHORIZATION) else {
        return challenge();
    };
    let Some((user, pass)) = parse_basic(hdr) else {
        return challenge();
    };

    // `[u8]: ConstantTimeEq` short-circuits on unequal lengths (see subtle
    // 2.x docs), so no separate length gate is needed.
    let user_matches: bool = user.as_bytes().ct_eq(expected.username.as_bytes()).into();
    if !user_matches {
        // Wrong-username requests otherwise return in microseconds while
        // wrong-password requests take ~100ms (argon2 on `spawn_blocking`).
        // An attacker timing 401s against different usernames can
        // enumerate the configured one. Mirror `http::auth::dummy_verify`
        // and run a throwaway argon2 verify against the real hash: the
        // result is discarded (no authentication ever happens on this
        // branch) so using the configured hash is safe and makes both
        // parse-time and verify-time latency match the happy path.
        let hash = expected.password_hash.clone();
        if let Err(e) = tokio::task::spawn_blocking(move || {
            if let Ok(parsed) = PasswordHash::new(&hash) {
                // Password bytes are fixed nonsense — the call exists
                // purely for latency parity, not authentication.
                let _ = Argon2::default().verify_password(b"::dummy::", &parsed);
            }
        })
        .await
        {
            // Swallowed and logged, not propagated: a 500 here would be
            // a stronger oracle than the one we're trying to close.
            tracing::warn!(error = %e, "metrics-auth dummy verify task failed");
        }
        return challenge();
    }

    // Argon2 verification is ~100ms of CPU work per call; a scrape burst or
    // a DoS of wrong-password attempts would otherwise starve every task
    // scheduled on this executor thread. Mirror the `http::auth` pattern:
    // move the whole parse+verify block onto the blocking pool. Cloning the
    // hash + password into the closure keeps the config snapshot stable
    // even if SIGHUP swaps the config mid-verify. `PasswordHash::new`
    // inside the closure is deliberate — the hash was PHC-validated at
    // config load, so a parse failure here means the runtime config drifted
    // from the validator; treat as non-match and 401 rather than leaking a
    // 500 to the scraper.
    let hash = expected.password_hash.clone();
    let pass_bytes = pass.into_bytes();
    let verified = tokio::task::spawn_blocking(move || match PasswordHash::new(&hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(&pass_bytes, &parsed)
            .is_ok(),
        Err(_) => false,
    })
    .await
    .unwrap_or(false);
    if !verified {
        return challenge();
    }

    next.run(req).await
}

/// 401 response with the `WWW-Authenticate` challenge Prometheus and curl
/// both understand. Emits a single uniform `warn!` so operators can spot a
/// misconfigured scraper server-side. The log line deliberately does NOT
/// distinguish the failure reason (missing header vs. wrong user vs. wrong
/// password vs. malformed): a fine-grained log would be a timing-oracle
/// signal leaked to anyone with access to the server logs.
fn challenge() -> Response {
    tracing::warn!(target: "meshmon::metrics_auth", "metrics auth failure");
    let mut resp = (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    resp.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static(r#"Basic realm="meshmon metrics""#),
    );
    resp
}

/// Parse an `Authorization: Basic <b64>` header into `(username, password)`.
/// Returns `None` on any malformed input: non-UTF-8 header, missing prefix,
/// invalid base64, non-UTF-8 decoded bytes, or missing colon separator.
///
/// The scheme token (`Basic`) is matched case-insensitively per RFC 7617
/// (section 2) and RFC 9110 (section 11.1): auth-scheme tokens are ABNF
/// tokens and MUST be treated as case-insensitive. A compliant proxy
/// that normalises to `basic <b64>` would otherwise get rejected.
fn parse_basic(h: &HeaderValue) -> Option<(String, String)> {
    let s = h.to_str().ok()?;
    // Split off exactly the five-char "Basic" token and require a
    // whitespace separator. Byte indexing (rather than `split_whitespace`)
    // keeps the base64 slice intact — whitespace INSIDE the b64 payload
    // would be malformed input and must not be silently collapsed.
    let (scheme, rest) = s.split_at_checked(5)?;
    if !scheme.eq_ignore_ascii_case("Basic") {
        return None;
    }
    // RFC 7235 §2.1 mandates 1*SP between scheme and credentials; real
    // proxies occasionally pad with a tab, so accept any leading ASCII
    // whitespace but reject an empty separator (e.g. `Basicabc...`).
    let after_ws = rest.trim_start_matches([' ', '\t']);
    if after_ws.len() == rest.len() {
        return None;
    }
    let decoded = STANDARD.decode(after_ws.trim()).ok()?;
    let decoded = String::from_utf8(decoded).ok()?;
    let (user, pass) = decoded.split_once(':')?;
    Some((user.to_owned(), pass.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode};
    use axum::routing::get;
    use axum::Router;
    use tower::util::ServiceExt;

    /// PHC hash of `CORRECT_PASSWORD` with `m=16,t=1,p=1` — matches
    /// `crate::http::auth::tests::TEST_HASH` and `tests/common::AUTH_TEST_HASH`.
    const PHC_HASH: &str =
        "$argon2id$v=19$m=16,t=1,p=1$c2FsdHNhbHQ$87ARSxtFrFp/0EGLYgzI7Giyu6y7PD1rUqoZugn3NqY";
    /// Password that hashes to [`PHC_HASH`]. Same value as
    /// `tests/common::AUTH_TEST_PASSWORD`.
    const CORRECT_PASSWORD: &str = "correct horse battery staple";

    fn state(with_auth: bool) -> AppState {
        let toml = if with_auth {
            format!(
                r#"
[database]
url = "postgres://ignored@h/d"

[probing]
udp_probe_secret = "hex:0011223344556677"

[service.metrics_auth]
username = "prom"
password_hash = "{PHC_HASH}"
"#
            )
        } else {
            r#"
[database]
url = "postgres://ignored@h/d"

[probing]
udp_probe_secret = "hex:0011223344556677"
"#
            .to_owned()
        };
        crate::config::test_state_from_toml(&toml)
    }

    fn app(state: AppState) -> Router {
        Router::new()
            .route("/metrics", get(|| async { "scraped" }))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                require_basic_auth,
            ))
            .with_state(state)
    }

    #[tokio::test]
    async fn passes_through_when_unconfigured() {
        let app = app(state(false));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rejects_missing_header_when_configured() {
        let app = app(state(true));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        assert!(
            resp.headers()
                .get(header::WWW_AUTHENTICATE)
                .is_some_and(|v| v.to_str().unwrap().starts_with("Basic")),
            "missing WWW-Authenticate challenge"
        );
    }

    #[tokio::test]
    async fn accepts_correct_credentials() {
        let app = app(state(true));
        let b64 = STANDARD.encode(format!("prom:{CORRECT_PASSWORD}"));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/metrics")
                    .header(header::AUTHORIZATION, format!("Basic {b64}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn rejects_wrong_password() {
        let app = app(state(true));
        let b64 = STANDARD.encode("prom:wrong");
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/metrics")
                    .header(header::AUTHORIZATION, format!("Basic {b64}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rejects_wrong_username() {
        let app = app(state(true));
        let b64 = STANDARD.encode(format!("someone-else:{CORRECT_PASSWORD}"));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/metrics")
                    .header(header::AUTHORIZATION, format!("Basic {b64}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// RFC 7235 §2.1 / RFC 9110 §11.1: auth-scheme tokens are case-insensitive.
    /// A compliant proxy that normalises to `basic <b64>` must still
    /// authenticate successfully.
    #[tokio::test]
    async fn parse_basic_accepts_lowercase_scheme() {
        let app = app(state(true));
        let b64 = STANDARD.encode(format!("prom:{CORRECT_PASSWORD}"));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/metrics")
                    .header(header::AUTHORIZATION, format!("basic {b64}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// `Basicabc...` (no separator between scheme and credentials) must be
    /// rejected so the case-insensitive rewrite doesn't accidentally
    /// accept concatenated garbage.
    #[tokio::test]
    async fn parse_basic_rejects_scheme_without_separator() {
        let app = app(state(true));
        let b64 = STANDARD.encode(format!("prom:{CORRECT_PASSWORD}"));
        let resp = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/metrics")
                    .header(header::AUTHORIZATION, format!("Basic{b64}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    /// Both wrong-username and wrong-password paths must run the argon2
    /// verify so request latency does not leak whether the configured
    /// username matched. Absolute latency is not asserted (argon2 speed
    /// varies wildly across CI executors) — instead the test averages
    /// several runs of each path and asserts the wrong-password/wrong-
    /// username ratio stays under 10×. Without the dummy verify the
    /// ratio is 100× or more (wrong-username returns in microseconds
    /// while wrong-password pays the full argon2 cost), so 10× is
    /// generous enough to absorb scheduler jitter on noisy CI while
    /// still catching a "dummy verify skipped" regression.
    #[tokio::test]
    async fn timing_oracle_flat_for_wrong_username() {
        use std::time::Instant;

        async fn measure_ms(app: Router, authz: String) -> u128 {
            let start = Instant::now();
            let resp = app
                .oneshot(
                    HttpRequest::builder()
                        .uri("/metrics")
                        .header(header::AUTHORIZATION, authz)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let elapsed = start.elapsed().as_micros();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
            elapsed
        }

        // Warm up — the first argon2 call on a fresh process pays a
        // one-time cost that would otherwise skew the baseline.
        let _ = measure_ms(
            app(state(true)),
            format!(
                "Basic {}",
                STANDARD.encode(format!("someone-else:{CORRECT_PASSWORD}"))
            ),
        )
        .await;

        const SAMPLES: u32 = 5;
        let mut wrong_user_total: u128 = 0;
        let mut wrong_pass_total: u128 = 0;
        for _ in 0..SAMPLES {
            wrong_user_total += measure_ms(
                app(state(true)),
                format!(
                    "Basic {}",
                    STANDARD.encode(format!("someone-else:{CORRECT_PASSWORD}"))
                ),
            )
            .await;
            wrong_pass_total += measure_ms(
                app(state(true)),
                format!("Basic {}", STANDARD.encode("prom:not-the-password")),
            )
            .await;
        }

        let wrong_user_avg = wrong_user_total / u128::from(SAMPLES);
        let wrong_pass_avg = wrong_pass_total / u128::from(SAMPLES);

        // Bail out defensively if either path ran in ~0µs (unrealistic
        // on any machine that can run argon2 at all) so the ratio test
        // below doesn't divide by zero or produce meaningless numbers.
        assert!(wrong_pass_avg > 0, "wrong-password baseline too fast");
        assert!(wrong_user_avg > 0, "wrong-username baseline too fast");

        // Without the dummy verify, wrong-username returns in ~10µs
        // (header parse + constant-time compare) while wrong-password
        // takes 100–1000µs+ for argon2 — a 10–100× gap. Requiring the
        // ratio to stay under 10× still catches that regression while
        // tolerating the scheduler jitter that inflates `wrong_pass_avg`
        // on loaded CI executors.
        let ratio = wrong_pass_avg as f64 / wrong_user_avg as f64;
        assert!(
            ratio < 10.0,
            "timing oracle regression: wrong-password avg {wrong_pass_avg}µs / \
             wrong-username avg {wrong_user_avg}µs = {ratio:.1}× — \
             dummy argon2 verify may not have run on the wrong-username path"
        );
    }
}

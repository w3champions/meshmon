//! `meshmon.toml` parsing + validation.
//!
//! # Sources
//!
//! Config is read from the path in `$MESHMON_CONFIG` (default
//! `/etc/meshmon/meshmon.toml`). Secrets (database URL, agent token) may be
//! stored inline (`url = "postgres://..."`) or indirected through env vars
//! (`url_env = "MESHMON_POSTGRES_URL"`). Inline wins if both are present; a
//! `*_env` pointing at an unset variable is a startup error.
//!
//! # Validation
//!
//! [`Config::from_str`] parses the TOML into a `RawConfig`, resolves env-var
//! indirections, validates (PHC hashes, required fields), and yields a
//! [`Config`] ready for the service to use.

use crate::error::BootError;
use password_hash::phc::PasswordHash;
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::Path;

/// Default path the service reads when `$MESHMON_CONFIG` is unset.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/meshmon/meshmon.toml";

/// Fully-validated service configuration.
///
/// Every reference-by-key from the TOML has been resolved: `url_env` entries
/// became inlined strings, password hashes have been sanity-parsed as PHC.
#[derive(Debug, Clone)]
pub struct Config {
    /// HTTP listener, shutdown deadline, and other transport-layer settings.
    pub service: ServiceSection,
    /// Postgres connection URL (resolved from inline or env var).
    pub database: DatabaseSection,
    /// Tracing/log filter + output format.
    pub logging: LoggingSection,
    /// Admin basic-auth users with PHC password hashes.
    pub auth: AuthSection,
    /// Shared-token auth settings for agent-facing endpoints.
    pub agent_api: AgentApiSection,
    /// Upstream service URLs (VictoriaMetrics, Alertmanager).
    pub upstream: UpstreamSection,
    /// Agent registry timing settings.
    pub agents: AgentsSection,
    /// Probing configuration broadcast to agents via `GetConfig`.
    pub probing: crate::probing::ProbingSection,
}

/// Transport-layer settings for the axum HTTP server.
#[derive(Debug, Clone)]
pub struct ServiceSection {
    /// Address `axum` binds. `0.0.0.0:8080` by default.
    pub listen_addr: SocketAddr,
    /// Optional — used by T09 to emit absolute URLs in responses. Kept here
    /// so config-reload observers can see changes in one place.
    pub public_base_url: Option<String>,
    /// Graceful-shutdown deadline. Tasks not completed in this window are
    /// aborted. 5s keeps pod rollouts snappy.
    pub shutdown_deadline: std::time::Duration,
    /// When `true`, trust `X-Forwarded-For`/`Forwarded` for client-IP
    /// extraction (used by the login rate limiter). Set `true` when behind
    /// an nginx reverse proxy; leave `false` for direct exposure — a
    /// misconfigured `true` lets attackers bypass per-IP rate limiting by
    /// forging the header.
    pub trust_forwarded_headers: bool,
    /// Optional HTTP Basic auth for `/metrics`. Unset → `/metrics` is
    /// unauthenticated (spec default). Set → the Basic auth middleware
    /// enforces credentials on scrape.
    pub metrics_auth: Option<MetricsAuthSection>,
}

/// Optional HTTP Basic credentials for `/metrics`. Unset → no auth
/// (the spec's default behavior).
#[derive(Debug, Clone)]
pub struct MetricsAuthSection {
    /// Basic-auth username the scraper must present.
    pub username: String,
    /// Fully resolved PHC-formatted argon2 hash. Already PHC-parsed at
    /// load time — validity is guaranteed if this struct exists.
    pub password_hash: String,
}

/// Resolved Postgres connection settings.
#[derive(Debug, Clone)]
pub struct DatabaseSection {
    resolved_url: String,
}

impl DatabaseSection {
    /// Borrow the resolved Postgres URL.
    pub fn url(&self) -> &str {
        &self.resolved_url
    }
}

/// Tracing-subscriber filter + output format.
#[derive(Debug, Clone)]
pub struct LoggingSection {
    /// `tracing-subscriber` env-filter directive (e.g. `info`, `debug,sqlx=warn`).
    pub filter: String,
    /// Whether to emit JSON records or compact single-line output.
    pub format: LogFormat,
}

/// Log output format. JSON is the deploy default; compact is for developer
/// tail-following and `RUST_LOG=debug` sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Structured JSON, one record per line.
    Json,
    /// Human-readable compact single-line format.
    Compact,
}

/// Admin basic-auth users configured in TOML.
#[derive(Debug, Clone, Default)]
pub struct AuthSection {
    /// Configured basic-auth principals. Empty by default.
    pub users: Vec<AuthUser>,
}

/// A single basic-auth principal with a pre-validated PHC hash.
#[derive(Debug, Clone)]
pub struct AuthUser {
    /// Basic-auth username (non-empty, validated at load).
    pub username: String,
    /// PHC-formatted hash string. Already parsed at load time — validity
    /// is guaranteed if this struct exists.
    pub password_hash: String,
}

/// Agent-facing API auth settings.
#[derive(Debug, Clone)]
pub struct AgentApiSection {
    /// Resolved shared bearer token for agent auth. `None` means agent
    /// endpoints are effectively disabled (returns 503) — useful for early
    /// deployments before the token secret is provisioned.
    pub shared_token: Option<String>,
    /// Per-IP rate limit (requests per minute). Default 60.
    pub rate_limit_per_minute: u32,
    /// Burst budget absorbed instantly before the sustained rate kicks in.
    /// Default 30 — sized for register + config + targets at agent startup
    /// (3 RPCs) plus headroom for 36 agents sharing a proxy.
    pub rate_limit_burst: u32,
    /// Optional TLS config for standalone mode (no proxy in front). When
    /// `None`, the service binds plaintext — appropriate behind nginx-proxy.
    pub tls: Option<AgentApiTls>,
}

impl Default for AgentApiSection {
    fn default() -> Self {
        Self {
            shared_token: None,
            rate_limit_per_minute: 60,
            rate_limit_burst: 30,
            tls: None,
        }
    }
}

/// TLS certificate + key paths for standalone agent-API mode.
#[derive(Debug, Clone)]
pub struct AgentApiTls {
    /// Path to the PEM-encoded certificate chain.
    pub cert_path: std::path::PathBuf,
    /// Path to the PEM-encoded private key.
    pub key_path: std::path::PathBuf,
}

/// URLs for upstream services that the meshmon service talks to.
#[derive(Debug, Clone, Default)]
pub struct UpstreamSection {
    /// VictoriaMetrics base URL, e.g. `http://meshmon-vm:8428`. Probed at
    /// startup with a warn-only outcome when unreachable (spec 03 §Startup).
    pub vm_url: Option<String>,
    /// Alertmanager base URL, e.g. `http://meshmon-alertmanager:9093`.
    pub alertmanager_url: Option<String>,
    /// Grafana base URL, e.g. `http://meshmon-grafana:3000`. Consumed by
    /// the transparent `/grafana/*` proxy. When unset, `/grafana/*`
    /// returns 503 and the SPA's iframe renders the broken-iframe state.
    pub grafana_url: Option<String>,
}

/// Agent registry knobs: how long a `last_seen_at` still counts as active,
/// how frequently the in-memory snapshot is re-read from Postgres.
#[derive(Debug, Clone)]
pub struct AgentsSection {
    /// Minutes after `last_seen_at` before an agent is no longer considered active.
    pub target_active_window_minutes: u32,
    /// How often (in seconds) the in-memory registry snapshot is refreshed from Postgres.
    pub refresh_interval_seconds: u32,
}

impl Default for AgentsSection {
    fn default() -> Self {
        Self {
            target_active_window_minutes: 5,
            refresh_interval_seconds: 10,
        }
    }
}

impl Default for ServiceSection {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::from(([0, 0, 0, 0], 8080)),
            public_base_url: None,
            shutdown_deadline: std::time::Duration::from_secs(5),
            trust_forwarded_headers: false,
            metrics_auth: None,
        }
    }
}

impl Default for LoggingSection {
    fn default() -> Self {
        Self {
            filter: "info".to_string(),
            format: LogFormat::Json,
        }
    }
}

// ---------- on-disk shape (serde) ----------

#[derive(Debug, Deserialize)]
struct RawConfig {
    #[serde(default)]
    service: RawService,
    database: RawDatabase,
    #[serde(default)]
    logging: RawLogging,
    #[serde(default)]
    auth: RawAuth,
    #[serde(default)]
    agent_api: RawAgentApi,
    #[serde(default)]
    upstream: RawUpstream,
    #[serde(default)]
    agents: RawAgentsSection,
    #[serde(default)]
    probing: crate::probing::RawProbingSection,
}

#[derive(Debug, Default, Deserialize)]
struct RawService {
    listen_addr: Option<String>,
    public_base_url: Option<String>,
    shutdown_deadline_seconds: Option<u64>,
    trust_forwarded_headers: Option<bool>,
    #[serde(default)]
    metrics_auth: Option<RawMetricsAuthSection>,
}

#[derive(Debug, Deserialize)]
struct RawMetricsAuthSection {
    username: String,
    #[serde(default)]
    password_hash: Option<String>,
    #[serde(default)]
    password_hash_env: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawDatabase {
    url: Option<String>,
    url_env: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawLogging {
    filter: Option<String>,
    format: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawAuth {
    #[serde(default)]
    users: Vec<RawUser>,
}

#[derive(Debug, Deserialize)]
struct RawUser {
    username: String,
    password_hash: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawAgentApi {
    shared_token: Option<String>,
    shared_token_env: Option<String>,
    rate_limit_per_minute: Option<u32>,
    rate_limit_burst: Option<u32>,
    tls: Option<RawAgentApiTls>,
}

#[derive(Debug, Deserialize)]
struct RawAgentApiTls {
    cert_path: std::path::PathBuf,
    key_path: std::path::PathBuf,
}

#[derive(Debug, Default, Deserialize)]
struct RawUpstream {
    vm_url: Option<String>,
    alertmanager_url: Option<String>,
    alertmanager_url_env: Option<String>,
    grafana_url: Option<String>,
    grafana_url_env: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawAgentsSection {
    #[serde(default = "default_target_active_window_minutes")]
    target_active_window_minutes: u32,
    #[serde(default = "default_refresh_interval_seconds")]
    refresh_interval_seconds: u32,
}

impl Default for RawAgentsSection {
    fn default() -> Self {
        Self {
            target_active_window_minutes: default_target_active_window_minutes(),
            refresh_interval_seconds: default_refresh_interval_seconds(),
        }
    }
}

fn default_target_active_window_minutes() -> u32 {
    5
}
fn default_refresh_interval_seconds() -> u32 {
    10
}

// ---------- loader ----------

impl Config {
    /// Load config from disk and validate. `path` is used verbatim in error
    /// messages; it need not exist on disk except for reading.
    pub fn from_file(path: &Path) -> Result<Self, BootError> {
        let path_str = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|source| BootError::ConfigRead {
            path: path_str.clone(),
            source,
        })?;
        Self::from_str(&text, &path_str)
    }

    /// Parse + validate in-memory. The `path` argument flows into error
    /// messages only; it doesn't need to exist.
    pub fn from_str(text: &str, path: &str) -> Result<Self, BootError> {
        let raw: RawConfig = toml::from_str(text).map_err(|source| BootError::ConfigParse {
            path: path.to_string(),
            source,
        })?;
        Self::try_from_raw(raw, path)
    }

    fn try_from_raw(raw: RawConfig, path: &str) -> Result<Self, BootError> {
        // --- service section ---
        let defaults = ServiceSection::default();
        let listen_addr = match raw.service.listen_addr.as_deref() {
            Some(s) => s.parse().map_err(|_| BootError::ConfigInvalid {
                path: path.to_string(),
                reason: format!("service.listen_addr `{s}` is not a valid SocketAddr"),
            })?,
            None => defaults.listen_addr,
        };
        let shutdown_deadline = match raw.service.shutdown_deadline_seconds {
            Some(0) => {
                return Err(BootError::ConfigInvalid {
                    path: path.to_string(),
                    reason: "service.shutdown_deadline_seconds must be > 0 — 0 aborts \
                             drain immediately; omit the key to get the default 5s"
                        .to_string(),
                });
            }
            Some(n) => std::time::Duration::from_secs(n),
            None => defaults.shutdown_deadline,
        };
        let metrics_auth = resolve_metrics_auth(raw.service.metrics_auth, path)?;
        let service = ServiceSection {
            listen_addr,
            public_base_url: raw.service.public_base_url,
            shutdown_deadline,
            trust_forwarded_headers: raw.service.trust_forwarded_headers.unwrap_or(false),
            metrics_auth,
        };

        // --- database section ---
        let resolved_url =
            resolve_secret(raw.database.url, raw.database.url_env, "database.url", path)?;
        let database = DatabaseSection { resolved_url };

        // --- logging section ---
        let filter = raw
            .logging
            .filter
            .unwrap_or_else(|| LoggingSection::default().filter);
        let format = match raw.logging.format.as_deref() {
            None | Some("json") => LogFormat::Json,
            Some("compact") => LogFormat::Compact,
            Some(other) => {
                return Err(BootError::ConfigInvalid {
                    path: path.to_string(),
                    reason: format!("logging.format `{other}` is not one of json|compact"),
                });
            }
        };
        let logging = LoggingSection { filter, format };

        // --- auth section (PHC validation) ---
        let mut users = Vec::with_capacity(raw.auth.users.len());
        for (idx, u) in raw.auth.users.into_iter().enumerate() {
            if u.username.trim().is_empty() {
                return Err(BootError::ConfigInvalid {
                    path: path.to_string(),
                    reason: format!("auth.users[{idx}].username is empty"),
                });
            }
            // Usernames are inserted into `X-WEBAUTH-USER` by the Grafana proxy.
            // Reject any byte `HeaderValue::try_from` would reject so a
            // CR/LF/NUL/DEL in a TOML basic string (e.g. `"alice\nevil"`)
            // fails at config load instead of panicking on the first
            // authenticated Grafana request.
            if u.username
                .bytes()
                .any(|b| (b < 0x20 && b != 0x09) || b == 0x7F)
            {
                return Err(BootError::ConfigInvalid {
                    path: path.to_string(),
                    reason: format!(
                        "auth.users[{idx}].username contains control bytes invalid in HTTP header values"
                    ),
                });
            }
            PasswordHash::new(&u.password_hash).map_err(|e| BootError::ConfigInvalid {
                path: path.to_string(),
                reason: format!("auth.users[{idx}].password_hash is not a valid PHC string: {e}"),
            })?;
            users.push(AuthUser {
                username: u.username,
                password_hash: u.password_hash,
            });
        }
        let auth = AuthSection { users };

        // --- agent api section ---
        let shared_token = resolve_optional_secret(
            raw.agent_api.shared_token,
            raw.agent_api.shared_token_env,
            "agent_api.shared_token",
            path,
        )?;
        let defaults = AgentApiSection::default();
        let rate_limit_per_minute = raw
            .agent_api
            .rate_limit_per_minute
            .unwrap_or(defaults.rate_limit_per_minute);
        if rate_limit_per_minute == 0 {
            return Err(BootError::ConfigInvalid {
                path: path.to_string(),
                reason: "agent_api.rate_limit_per_minute must be > 0".to_string(),
            });
        }
        let rate_limit_burst = raw
            .agent_api
            .rate_limit_burst
            .unwrap_or(defaults.rate_limit_burst);
        if rate_limit_burst == 0 {
            return Err(BootError::ConfigInvalid {
                path: path.to_string(),
                reason: "agent_api.rate_limit_burst must be > 0".to_string(),
            });
        }
        let tls = raw.agent_api.tls.map(|t| AgentApiTls {
            cert_path: t.cert_path,
            key_path: t.key_path,
        });
        let agent_api = AgentApiSection {
            shared_token,
            rate_limit_per_minute,
            rate_limit_burst,
            tls,
        };

        // --- upstream section ---
        let alertmanager_url = resolve_optional_secret(
            raw.upstream.alertmanager_url,
            raw.upstream.alertmanager_url_env,
            "upstream.alertmanager_url",
            path,
        )?;
        let grafana_url = resolve_optional_secret(
            raw.upstream.grafana_url,
            raw.upstream.grafana_url_env,
            "upstream.grafana_url",
            path,
        )?;
        for (url, key) in [
            (alertmanager_url.as_deref(), "upstream.alertmanager_url"),
            (grafana_url.as_deref(), "upstream.grafana_url"),
        ] {
            if let Some(u) = url {
                reject_self_referential_upstream(u, &service.listen_addr, key, path)?;
            }
        }
        let upstream = UpstreamSection {
            vm_url: raw.upstream.vm_url,
            alertmanager_url,
            grafana_url,
        };

        // --- agents section ---
        if raw.agents.target_active_window_minutes == 0 {
            return Err(BootError::ConfigInvalid {
                path: path.to_string(),
                reason: "agents.target_active_window_minutes must be non-zero".to_string(),
            });
        }
        if raw.agents.refresh_interval_seconds == 0 {
            return Err(BootError::ConfigInvalid {
                path: path.to_string(),
                reason: "agents.refresh_interval_seconds must be non-zero".to_string(),
            });
        }
        let agents = AgentsSection {
            target_active_window_minutes: raw.agents.target_active_window_minutes,
            refresh_interval_seconds: raw.agents.refresh_interval_seconds,
        };

        // --- probing section ---
        let probing = crate::probing::ProbingSection::try_from(raw.probing).map_err(|reason| {
            BootError::ConfigInvalid {
                path: path.to_string(),
                reason: format!("[probing] {reason}"),
            }
        })?;

        Ok(Config {
            service,
            database,
            logging,
            auth,
            agent_api,
            upstream,
            agents,
            probing,
        })
    }
}

/// Resolve a required secret: prefer inline, fall back to env var, error if
/// neither is set. An empty string (inline `""` or `VAR=""` in the
/// environment) is treated as "not set" — a blank secret is almost always
/// a configuration mistake, and we'd rather fail loudly at boot than
/// surface a cryptic downstream error (bad DB URL, empty auth token) once
/// the service is already serving traffic.
fn resolve_secret(
    inline: Option<String>,
    env_name: Option<String>,
    key: &str,
    path: &str,
) -> Result<String, BootError> {
    if let Some(v) = inline.filter(|s| !s.is_empty()) {
        return Ok(v);
    }
    if let Some(name) = env_name {
        return match std::env::var(&name) {
            Ok(v) if !v.is_empty() => Ok(v),
            Ok(_) => Err(BootError::ConfigInvalid {
                path: path.to_string(),
                reason: format!("env var {name} (for {key}) is set but empty"),
            }),
            Err(_) => Err(BootError::EnvMissing {
                name,
                key: key.to_string(),
            }),
        };
    }
    Err(BootError::ConfigInvalid {
        path: path.to_string(),
        reason: format!("neither {key} nor {key}_env is set"),
    })
}

/// Resolve an optional secret: absent keys yield `None`; present-but-unset
/// env var is an error (an operator opted-in and typo'd the env var name).
/// An empty env-var value is also rejected — same rationale as
/// [`resolve_secret`].
fn resolve_optional_secret(
    inline: Option<String>,
    env_name: Option<String>,
    key: &str,
    path: &str,
) -> Result<Option<String>, BootError> {
    if let Some(v) = inline.filter(|s| !s.is_empty()) {
        return Ok(Some(v));
    }
    if let Some(name) = env_name {
        return match std::env::var(&name) {
            Ok(v) if !v.is_empty() => Ok(Some(v)),
            Ok(_) => Err(BootError::ConfigInvalid {
                path: path.to_string(),
                reason: format!("env var {name} (for {key}) is set but empty"),
            }),
            Err(_) => Err(BootError::EnvMissing {
                name,
                key: key.to_string(),
            }),
        };
    }
    Ok(None)
}

/// Reject an upstream URL that would cause the proxy handler to loop
/// back into its own listen socket. A misconfigured operator who sets
/// `grafana_url = "http://meshmon.example.com/grafana"` (the public
/// URL) instead of the internal address would recurse forever.
///
/// Only rejects when scheme+host+port exactly match the service's own
/// listen address (or `0.0.0.0` / `[::]` expanded to `127.0.0.1` /
/// `[::1]` for bind-all cases).
fn reject_self_referential_upstream(
    raw_url: &str,
    listen_addr: &std::net::SocketAddr,
    key: &str,
    path: &str,
) -> Result<(), BootError> {
    let url = url::Url::parse(raw_url).map_err(|e| BootError::ConfigInvalid {
        path: path.to_string(),
        reason: format!("{key} `{raw_url}` is not a valid URL: {e}"),
    })?;
    let Some(host) = url.host_str() else {
        return Ok(());
    };
    let port = url.port_or_known_default();
    let Some(port) = port else { return Ok(()) };

    if port != listen_addr.port() {
        return Ok(());
    }

    let listen_hosts: &[&str] = match listen_addr.ip() {
        std::net::IpAddr::V4(v4) if v4.is_unspecified() => &["127.0.0.1", "localhost"],
        // IPv6 unspecified (`[::]`) is dual-stack on most OSes: an
        // IPv4 client connecting to `127.0.0.1:<port>` lands on the
        // same listener via v4-mapped-v6. Treat the v4 loopback as
        // self too.
        std::net::IpAddr::V6(v6) if v6.is_unspecified() => &["::1", "127.0.0.1", "localhost"],
        // Explicit loopback binds still recurse when the operator
        // writes `localhost` (or the other-family loopback alias) in
        // the upstream URL — resolver translates both to the same
        // local listener.
        std::net::IpAddr::V4(v4) if v4.is_loopback() => &["::1", "localhost"],
        std::net::IpAddr::V6(v6) if v6.is_loopback() => &["127.0.0.1", "localhost"],
        _ => &[],
    };
    let own_ip = listen_addr.ip().to_string();
    if host == own_ip || listen_hosts.contains(&host) {
        return Err(BootError::ConfigInvalid {
            path: path.to_string(),
            reason: format!(
                "{key} `{raw_url}` recurses into meshmon's own listen address \
                 ({listen_addr}); set it to the upstream's internal address instead"
            ),
        });
    }
    Ok(())
}

/// Resolve the optional `[service.metrics_auth]` block.
///
/// Absent → `None` (spec default: `/metrics` unauthenticated). Present → the
/// hash is either inlined or read from an env var, then PHC-parsed so a
/// malformed hash fails fast at startup, not on the first 401 attempt.
fn resolve_metrics_auth(
    raw: Option<RawMetricsAuthSection>,
    path: &str,
) -> Result<Option<MetricsAuthSection>, BootError> {
    let Some(raw) = raw else { return Ok(None) };
    if raw.username.trim().is_empty() {
        return Err(BootError::ConfigInvalid {
            path: path.to_string(),
            reason: "[service.metrics_auth].username must not be empty".to_string(),
        });
    }
    let hash = match (raw.password_hash, raw.password_hash_env) {
        (Some(h), None) if !h.trim().is_empty() => h,
        (None, Some(env)) => match std::env::var(&env) {
            Ok(v) if !v.trim().is_empty() => v,
            Ok(_) => {
                return Err(BootError::ConfigInvalid {
                    path: path.to_string(),
                    reason: format!(
                        "[service.metrics_auth].password_hash_env={env} resolved to empty string"
                    ),
                });
            }
            Err(_) => {
                return Err(BootError::EnvMissing {
                    name: env,
                    key: "service.metrics_auth.password_hash".to_string(),
                });
            }
        },
        (Some(_), Some(_)) => {
            return Err(BootError::ConfigInvalid {
                path: path.to_string(),
                reason: "[service.metrics_auth] set both password_hash and password_hash_env"
                    .to_string(),
            });
        }
        _ => {
            return Err(BootError::ConfigInvalid {
                path: path.to_string(),
                reason: "[service.metrics_auth] requires password_hash or password_hash_env"
                    .to_string(),
            });
        }
    };
    // PHC sanity-parse so a malformed hash fails fast at startup, not on
    // the first 401 attempt.
    PasswordHash::new(&hash).map_err(|e| BootError::ConfigInvalid {
        path: path.to_string(),
        reason: format!("[service.metrics_auth].password_hash is not a valid PHC string: {e}"),
    })?;
    Ok(Some(MetricsAuthSection {
        username: raw.username,
        password_hash: hash,
    }))
}

/// Construct an [`AppState`](crate::state::AppState) from raw TOML for unit
/// tests that only need a parsed [`Config`] plumbed through state — for
/// example the `/metrics` Basic-auth middleware, which dispatches on
/// `cfg.service.metrics_auth` and ignores every other field.
///
/// Differences from the integration-test harness in `tests/common/mod.rs`:
///
/// - Synchronous (plain `#[test]`-compatible). Unit tests use `#[tokio::test]`
///   on the middleware itself, but callers may also want to build state in
///   non-async helpers without lifting them.
/// - Pool is [`sqlx::PgPool::connect_lazy`] — never opens a socket. Tests
///   that need real DB access should use the integration-test harness.
/// - Ingestion workers are spawned with a pre-cancelled
///   [`CancellationToken`](tokio_util::sync::CancellationToken) so they
///   exit immediately without doing any DB work.
/// - The Prometheus recorder is installed process-wide via
///   [`crate::metrics::test_install`], which dedups across every test in
///   the same binary (a second `metrics::set_global_recorder` call would
///   panic).
#[cfg(test)]
pub(crate) fn test_state_from_toml(toml: &str) -> crate::state::AppState {
    use crate::ingestion::{IngestionConfig, IngestionPipeline};
    use crate::registry::AgentRegistry;
    use crate::state::AppState;
    use arc_swap::ArcSwap;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::watch;
    use tokio_util::sync::CancellationToken;

    let cfg = Arc::new(Config::from_str(toml, "unit-test.toml").expect("parse"));
    let swap = Arc::new(ArcSwap::from(cfg.clone()));
    let (_tx, rx) = watch::channel(cfg);

    // Lazy pool: `connect_lazy` never opens a socket, so tests that never
    // touch the DB (middleware, handler shape tests) stay hermetic.
    let pool =
        sqlx::PgPool::connect_lazy("postgres://ignored@127.0.0.1/ignored").expect("lazy pool");

    // Pre-cancel so the ingestion workers exit on first poll without
    // attempting any DB work.
    let token = CancellationToken::new();
    token.cancel();
    let ingestion = IngestionPipeline::spawn(
        IngestionConfig::default_with_url("http://127.0.0.1:1".into()),
        pool.clone(),
        token,
    );

    let registry = Arc::new(AgentRegistry::new(
        pool.clone(),
        Duration::from_secs(60),
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

#[cfg(test)]
mod tests {
    use super::*;

    const MIN_TOML: &str = r#"
[database]
url = "postgres://ignored@h/d"

[probing]
udp_probe_secret = "hex:0011223344556677"
"#;

    #[test]
    fn upstream_grafana_url_parses_from_inline_value() {
        let toml =
            format!("{MIN_TOML}\n[upstream]\ngrafana_url = \"http://meshmon-grafana:3000\"\n");
        let cfg = Config::from_str(&toml, "t.toml").expect("parse");
        assert_eq!(
            cfg.upstream.grafana_url.as_deref(),
            Some("http://meshmon-grafana:3000")
        );
    }

    #[test]
    fn upstream_grafana_url_resolves_from_env_indirection() {
        std::env::set_var(
            "MESHMON_TEST_GRAFANA_URL_XYZ",
            "http://grafana.internal:3000",
        );
        let toml =
            format!("{MIN_TOML}\n[upstream]\ngrafana_url_env = \"MESHMON_TEST_GRAFANA_URL_XYZ\"\n");
        let cfg = Config::from_str(&toml, "t.toml").expect("parse");
        assert_eq!(
            cfg.upstream.grafana_url.as_deref(),
            Some("http://grafana.internal:3000")
        );
        std::env::remove_var("MESHMON_TEST_GRAFANA_URL_XYZ");
    }

    #[test]
    fn upstream_url_pointing_at_self_listen_addr_is_rejected() {
        let toml = format!(
            r#"{MIN_TOML}
[service]
listen_addr = "127.0.0.1:8080"

[upstream]
grafana_url = "http://127.0.0.1:8080"
"#
        );
        let err = Config::from_str(&toml, "t.toml").expect_err("must reject");
        match err {
            BootError::ConfigInvalid { reason, .. } => {
                assert!(
                    reason.contains("recurs"),
                    "expected recursion-guard message, got: {reason}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn upstream_localhost_on_loopback_listen_is_rejected() {
        // Common operator footgun: bind to 127.0.0.1 and point the
        // upstream at `http://localhost:<same-port>/...`. The names
        // are different strings but resolve to the same socket, so
        // the proxy still recurses into itself.
        let toml = format!(
            r#"{MIN_TOML}
[service]
listen_addr = "127.0.0.1:8080"

[upstream]
grafana_url = "http://localhost:8080/grafana"
"#
        );
        let err = Config::from_str(&toml, "t.toml").expect_err("must reject");
        match err {
            BootError::ConfigInvalid { reason, .. } => {
                assert!(
                    reason.contains("recurs"),
                    "expected recursion-guard message, got: {reason}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn upstream_alt_loopback_family_on_loopback_listen_is_rejected() {
        // Binding to ::1 while setting the upstream to 127.0.0.1
        // (or vice-versa) is the same recursion by a different name.
        let toml = format!(
            r#"{MIN_TOML}
[service]
listen_addr = "[::1]:8080"

[upstream]
grafana_url = "http://127.0.0.1:8080/grafana"
"#
        );
        let err = Config::from_str(&toml, "t.toml").expect_err("must reject");
        assert!(matches!(err, BootError::ConfigInvalid { .. }));
    }

    #[test]
    fn upstream_v4_loopback_on_ipv6_unspecified_listen_is_rejected() {
        // Dual-stack `[::]` listeners accept IPv4 traffic via
        // v4-mapped-v6, so `http://127.0.0.1:<same-port>` still
        // recurses into meshmon.
        let toml = format!(
            r#"{MIN_TOML}
[service]
listen_addr = "[::]:8080"

[upstream]
grafana_url = "http://127.0.0.1:8080/grafana"
"#
        );
        let err = Config::from_str(&toml, "t.toml").expect_err("must reject");
        assert!(matches!(err, BootError::ConfigInvalid { .. }));
    }

    #[test]
    fn alertmanager_upstream_pointing_at_self_is_rejected() {
        let toml = format!(
            r#"{MIN_TOML}
[service]
listen_addr = "0.0.0.0:8080"

[upstream]
alertmanager_url = "http://0.0.0.0:8080/alertmanager"
"#
        );
        let err = Config::from_str(&toml, "t.toml").expect_err("must reject");
        assert!(matches!(err, BootError::ConfigInvalid { .. }));
    }

    #[test]
    fn username_with_control_chars_is_rejected() {
        // A TOML basic string with `\n` produces a literal LF byte in the
        // parsed username. Without the header-value validation at load,
        // this slips through and panics later when the Grafana proxy
        // builds `X-WEBAUTH-USER`.
        let valid_hash =
            "$argon2id$v=19$m=16,t=1,p=1$c2FsdHNhbHQ$87ARSxtFrFp/0EGLYgzI7Giyu6y7PD1rUqoZugn3NqY";
        let toml = format!(
            "{MIN_TOML}\n[[auth.users]]\nusername = \"alice\\nevil\"\npassword_hash = \"{valid_hash}\"\n"
        );
        let err = Config::from_str(&toml, "t.toml").expect_err("must reject");
        match err {
            BootError::ConfigInvalid { reason, .. } => {
                assert!(
                    reason.contains("control bytes"),
                    "expected control-bytes message, got: {reason}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}

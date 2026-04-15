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
#[derive(Debug, Clone, Default)]
pub struct AgentApiSection {
    /// Resolved shared bearer token for agent auth. `None` means agent
    /// endpoints are effectively disabled (returns 503) — useful for early
    /// deployments before the token secret is provisioned.
    pub shared_token: Option<String>,
}

/// URLs for upstream services that the meshmon service talks to.
#[derive(Debug, Clone, Default)]
pub struct UpstreamSection {
    /// VictoriaMetrics base URL, e.g. `http://meshmon-vm:8428`. Probed at
    /// startup with a warn-only outcome when unreachable (spec 03 §Startup).
    pub vm_url: Option<String>,
    /// Alertmanager base URL, e.g. `http://meshmon-alertmanager:9093`.
    pub alertmanager_url: Option<String>,
}

impl Default for ServiceSection {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::from(([0, 0, 0, 0], 8080)),
            public_base_url: None,
            shutdown_deadline: std::time::Duration::from_secs(5),
            trust_forwarded_headers: false,
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
}

#[derive(Debug, Default, Deserialize)]
struct RawService {
    listen_addr: Option<String>,
    public_base_url: Option<String>,
    shutdown_deadline_seconds: Option<u64>,
    trust_forwarded_headers: Option<bool>,
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
}

#[derive(Debug, Default, Deserialize)]
struct RawUpstream {
    vm_url: Option<String>,
    alertmanager_url: Option<String>,
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
        let service = ServiceSection {
            listen_addr,
            public_base_url: raw.service.public_base_url,
            shutdown_deadline,
            trust_forwarded_headers: raw.service.trust_forwarded_headers.unwrap_or(false),
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
        )?;
        let agent_api = AgentApiSection { shared_token };

        // --- upstream section ---
        let upstream = UpstreamSection {
            vm_url: raw.upstream.vm_url,
            alertmanager_url: raw.upstream.alertmanager_url,
        };

        Ok(Config {
            service,
            database,
            logging,
            auth,
            agent_api,
            upstream,
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
) -> Result<Option<String>, BootError> {
    if let Some(v) = inline.filter(|s| !s.is_empty()) {
        return Ok(Some(v));
    }
    if let Some(name) = env_name {
        return match std::env::var(&name) {
            Ok(v) if !v.is_empty() => Ok(Some(v)),
            Ok(_) => Err(BootError::EnvMissing {
                name,
                key: key.to_string(),
            }),
            Err(_) => Err(BootError::EnvMissing {
                name,
                key: key.to_string(),
            }),
        };
    }
    Ok(None)
}

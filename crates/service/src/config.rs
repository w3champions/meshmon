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
    /// Enrichment provider configuration for the IP catalogue.
    pub enrichment: EnrichmentSection,
    /// Campaigns scheduler + size-preview configuration.
    pub campaigns: CampaignsSection,
}

/// Campaigns scheduler, size-guard, and per-destination-rate-limit settings.
#[derive(Debug, Clone)]
pub struct CampaignsSection {
    /// Enable the background scheduler. Defaults to `false` until a
    /// real prober ships: T45 wires the transport but agents still use
    /// `StubProber` (the real trippy-backed prober lands in T46).
    /// Flipping this on before T46 would persist synthetic measurements
    /// against real campaigns. Operators who need the scheduler off
    /// even after T46 can keep this `false` in their config.
    pub enabled: bool,
    /// Soft warning threshold on the composer's expected-dispatch count.
    /// Above this the frontend shows a confirm dialog. No hard cap.
    pub size_warning_threshold: u32,
    /// Scheduler tick interval in milliseconds. LISTEN/NOTIFY wakes the
    /// scheduler early on state changes; the tick is the periodic
    /// fallback.
    pub scheduler_tick_ms: u32,
    /// Maximum dispatch attempts per pair before it becomes `skipped`
    /// with `last_error='agent_offline'`.
    pub max_pair_attempts: u16,
    /// Soft per-destination rate limit applied by the scheduler's token
    /// bucket. T45 replaces this with the dispatch-transport's own
    /// granularity; T44 uses it as the initial value.
    pub per_destination_rps: u32,
    /// Cluster-wide default per-agent concurrent-measurement cap. A
    /// `RegisterRequest.campaign_max_concurrency` override persisted on
    /// `agents.campaign_max_concurrency` wins per agent.
    pub default_agent_concurrency: u32,
    /// Maximum number of targets in a single
    /// `AgentCommand.RunMeasurementBatch` RPC. Caps `chunk_size` on
    /// `take_pending_batch` so one RPC cannot monopolise an agent.
    pub max_batch_size: u32,
}

impl Default for CampaignsSection {
    fn default() -> Self {
        Self {
            enabled: false,
            size_warning_threshold: 1000,
            scheduler_tick_ms: 500,
            max_pair_attempts: 3,
            per_destination_rps: 2,
            default_agent_concurrency: 16,
            max_batch_size: 50,
        }
    }
}

/// Enrichment provider chain configuration.
///
/// Each sub-section toggles one concrete provider. The catalogue
/// runner walks providers in a fixed order and skips any whose
/// `enabled` flag is `false`.
#[derive(Debug, Clone, Default)]
pub struct EnrichmentSection {
    /// IPGeolocation.io — paid geo / ASN / network operator. ToS-gated.
    pub ipgeolocation: IpGeolocationSection,
    /// RDAP — free, registry-maintained allocation metadata.
    pub rdap: RdapSection,
    /// MaxMind GeoLite2 — local mmdb databases (opt-in, feature-flagged).
    pub maxmind: MaxmindSection,
    /// WHOIS — legacy ASN / netblock fallback (opt-in, feature-flagged).
    pub whois: WhoisSection,
}

/// IPGeolocation.io provider settings.
///
/// The free tier imposes a terms-of-service acknowledgement: the
/// service refuses to start with `enabled = true` unless the operator
/// explicitly sets `acknowledged_tos = true` (see
/// `docs/campaigns/architecture.md`).
#[derive(Debug, Clone, Default)]
pub struct IpGeolocationSection {
    /// Whether to invoke this provider during enrichment.
    pub enabled: bool,
    /// Resolved API key (from inline `api_key` or `api_key_env`). `None`
    /// only when `enabled = false` — the loader rejects `enabled = true`
    /// with no key present.
    pub api_key: Option<String>,
    /// Operator's explicit acknowledgement of the provider's terms of
    /// service. Must be `true` when `enabled = true`.
    pub acknowledged_tos: bool,
}

/// RDAP provider settings. Enabled by default. The provider issues a
/// bootstrapped RDAP request via `icann-rdap-client` (IANA → RIR) and
/// needs no API key. See [`rdap_enabled_default`] for details on what
/// the provider populates.
#[derive(Debug, Clone)]
pub struct RdapSection {
    /// Whether to invoke this provider during enrichment.
    pub enabled: bool,
}

impl Default for RdapSection {
    fn default() -> Self {
        Self {
            enabled: rdap_enabled_default(),
        }
    }
}

/// MaxMind GeoLite2 provider settings. Requires the
/// `enrichment-maxmind` feature flag at build time; the paths point to
/// operator-provided mmdb files.
#[derive(Debug, Clone, Default)]
pub struct MaxmindSection {
    /// Whether to invoke this provider during enrichment.
    pub enabled: bool,
    /// Path to the GeoLite2 City mmdb file.
    pub city_mmdb: Option<std::path::PathBuf>,
    /// Path to the GeoLite2 ASN mmdb file.
    pub asn_mmdb: Option<std::path::PathBuf>,
}

/// WHOIS provider settings. Requires the `enrichment-whois` feature
/// flag at build time.
#[derive(Debug, Clone, Default)]
pub struct WhoisSection {
    /// Whether to invoke this provider during enrichment.
    pub enabled: bool,
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
    /// Whether to set the `Secure` attribute on the session cookie.
    /// Defaults to `true` (the safe default for production). Flip to
    /// `false` only when serving over plain HTTP — browsers silently
    /// drop `Secure` cookies received over HTTP, which breaks login.
    pub session_cookie_secure: bool,
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
            session_cookie_secure: true,
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
    #[serde(default)]
    enrichment: RawEnrichmentSection,
    #[serde(default)]
    campaigns: RawCampaigns,
}

#[derive(Debug, Deserialize, Default)]
struct RawCampaigns {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    size_warning_threshold: Option<u32>,
    #[serde(default)]
    scheduler_tick_ms: Option<u32>,
    #[serde(default)]
    max_pair_attempts: Option<u16>,
    #[serde(default)]
    per_destination_rps: Option<u32>,
    #[serde(default)]
    default_agent_concurrency: Option<u32>,
    #[serde(default)]
    max_batch_size: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
struct RawEnrichmentSection {
    #[serde(default)]
    ipgeolocation: RawIpGeolocationSection,
    #[serde(default)]
    rdap: RawRdapSection,
    #[serde(default)]
    maxmind: RawMaxmindSection,
    #[serde(default)]
    whois: RawWhoisSection,
}

#[derive(Debug, Default, Deserialize)]
struct RawIpGeolocationSection {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    acknowledged_tos: bool,
}

#[derive(Debug, Deserialize)]
struct RawRdapSection {
    #[serde(default = "rdap_enabled_default")]
    enabled: bool,
}

impl Default for RawRdapSection {
    fn default() -> Self {
        Self {
            enabled: rdap_enabled_default(),
        }
    }
}

fn rdap_enabled_default() -> bool {
    // RDAP enrichment ships enabled out of the box. The provider drives
    // `rdap_bootstrapped_request` against IANA + the RIRs, extracts
    // net_name / country / organisation (and ASN on ARIN networks via the
    // `arin_originas0_originautnums` extension, falling back to WHOIS
    // elsewhere), and needs no API key. Operators who want to skip it can
    // set `[enrichment.rdap] enabled = false` in their config.
    true
}

#[derive(Debug, Default, Deserialize)]
struct RawMaxmindSection {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    city_mmdb: Option<std::path::PathBuf>,
    #[serde(default)]
    asn_mmdb: Option<std::path::PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct RawWhoisSection {
    #[serde(default)]
    enabled: bool,
}

#[derive(Debug, Default, Deserialize)]
struct RawService {
    listen_addr: Option<String>,
    public_base_url: Option<String>,
    shutdown_deadline_seconds: Option<u64>,
    trust_forwarded_headers: Option<bool>,
    session_cookie_secure: Option<bool>,
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
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    username_env: Option<String>,
    #[serde(default)]
    password_hash: Option<String>,
    #[serde(default)]
    password_hash_env: Option<String>,
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
            session_cookie_secure: raw.service.session_cookie_secure.unwrap_or(true),
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
            let username =
                resolve_auth_user_field("username", idx, u.username, u.username_env, path)?;
            // Usernames are inserted into `X-WEBAUTH-USER` by the Grafana
            // proxy via `HeaderValue::try_from`, which accepts only
            // visible ASCII (0x20–0x7E) plus tab. Reject everything else
            // at load so a control byte (`"alice\nevil"`) or a non-ASCII
            // codepoint (`"mário"`) fails fast instead of panicking on
            // the first authenticated Grafana request.
            if username
                .bytes()
                .any(|b| b >= 0x80 || b == 0x7F || (b < 0x20 && b != 0x09))
            {
                return Err(BootError::ConfigInvalid {
                    path: path.to_string(),
                    reason: format!(
                        "auth.users[{idx}].username must be visible ASCII \
                         (printable 0x20-0x7E, optionally tab) so it can \
                         be forwarded as `X-WEBAUTH-USER` to Grafana"
                    ),
                });
            }
            let password_hash = resolve_auth_user_field(
                "password_hash",
                idx,
                u.password_hash,
                u.password_hash_env,
                path,
            )?;
            PasswordHash::new(&password_hash).map_err(|e| BootError::ConfigInvalid {
                path: path.to_string(),
                reason: format!("auth.users[{idx}].password_hash is not a valid PHC string: {e}"),
            })?;
            users.push(AuthUser {
                username,
                password_hash,
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
                reject_self_referential_upstream(
                    u,
                    &service.listen_addr,
                    service.public_base_url.as_deref(),
                    key,
                    path,
                )?;
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

        // --- enrichment section ---
        let enrichment = resolve_enrichment(raw.enrichment, path)?;

        // --- campaigns section ---
        let campaigns_defaults = CampaignsSection::default();
        let campaigns = CampaignsSection {
            enabled: raw.campaigns.enabled.unwrap_or(campaigns_defaults.enabled),
            size_warning_threshold: raw
                .campaigns
                .size_warning_threshold
                .unwrap_or(campaigns_defaults.size_warning_threshold),
            scheduler_tick_ms: match raw.campaigns.scheduler_tick_ms {
                Some(0) => {
                    return Err(BootError::ConfigInvalid {
                        path: path.to_string(),
                        reason: "campaigns.scheduler_tick_ms must be > 0".to_string(),
                    });
                }
                Some(n) => n,
                None => campaigns_defaults.scheduler_tick_ms,
            },
            max_pair_attempts: match raw.campaigns.max_pair_attempts {
                Some(0) => {
                    return Err(BootError::ConfigInvalid {
                        path: path.to_string(),
                        reason: "campaigns.max_pair_attempts must be > 0".to_string(),
                    });
                }
                Some(n) => n,
                None => campaigns_defaults.max_pair_attempts,
            },
            per_destination_rps: match raw.campaigns.per_destination_rps {
                Some(0) => {
                    return Err(BootError::ConfigInvalid {
                        path: path.to_string(),
                        reason: "campaigns.per_destination_rps must be > 0".to_string(),
                    });
                }
                Some(n) => n,
                None => campaigns_defaults.per_destination_rps,
            },
            default_agent_concurrency: match raw.campaigns.default_agent_concurrency {
                Some(0) => {
                    return Err(BootError::ConfigInvalid {
                        path: path.to_string(),
                        reason: "campaigns.default_agent_concurrency must be > 0".to_string(),
                    });
                }
                Some(n) => n,
                None => campaigns_defaults.default_agent_concurrency,
            },
            max_batch_size: match raw.campaigns.max_batch_size {
                Some(0) => {
                    return Err(BootError::ConfigInvalid {
                        path: path.to_string(),
                        reason: "campaigns.max_batch_size must be > 0".to_string(),
                    });
                }
                Some(n) => n,
                None => campaigns_defaults.max_batch_size,
            },
        };

        Ok(Config {
            service,
            database,
            logging,
            auth,
            agent_api,
            upstream,
            agents,
            probing,
            enrichment,
            campaigns,
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

/// Resolve a required `[[auth.users]]` field that accepts either an
/// inline value or an `_env` indirection. Mutually exclusive; both-set
/// is rejected; neither-set is rejected.
///
/// Unlike [`resolve_secret`], whitespace-only values are rejected
/// (either inline or via env). `username` and `password_hash` are
/// never legitimately whitespace, so catching it at parse time gives
/// a specific error message instead of a cryptic PHC-validation or
/// `X-WEBAUTH-USER`-forwarding failure downstream.
fn resolve_auth_user_field(
    field: &str,
    idx: usize,
    inline: Option<String>,
    env_name: Option<String>,
    path: &str,
) -> Result<String, BootError> {
    match (inline, env_name) {
        (Some(v), None) if !v.trim().is_empty() => Ok(v),
        (Some(_), None) => Err(BootError::ConfigInvalid {
            path: path.to_string(),
            reason: format!("auth.users[{idx}].{field} is set to an empty string"),
        }),
        (None, Some(env)) => match std::env::var(&env) {
            Ok(v) if !v.trim().is_empty() => Ok(v),
            Ok(_) => Err(BootError::ConfigInvalid {
                path: path.to_string(),
                reason: format!("auth.users[{idx}].{field}_env={env} resolved to an empty string"),
            }),
            Err(_) => Err(BootError::EnvMissing {
                name: env,
                key: format!("auth.users[{idx}].{field}_env"),
            }),
        },
        (Some(_), Some(_)) => Err(BootError::ConfigInvalid {
            path: path.to_string(),
            reason: format!("auth.users[{idx}] set both {field} and {field}_env"),
        }),
        (None, None) => Err(BootError::ConfigInvalid {
            path: path.to_string(),
            reason: format!("auth.users[{idx}] requires {field} or {field}_env"),
        }),
    }
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
/// `grafana_url = "https://meshmon.example.com/grafana"` (the public
/// URL) instead of the internal address would recurse forever.
///
/// Checks two independent routes to the same listener:
///  1. URL scheme+host+port match the internal listen address. This
///     covers (a) exact matches against the bound IP; (b) the loopback
///     / `localhost` aliases that resolve to it; and (c) when bound to
///     a wildcard (`0.0.0.0` / `[::]`), every local interface IP
///     enumerated via [`host_matches_any_local_interface`].
///  2. URL host + scheme-normalized port match
///     `service.public_base_url`, which identifies the externally
///     advertised URL — the one that resolves back through the edge
///     proxy onto this listener. Port normalization (via
///     `port_or_known_default`) covers the typical nginx case where
///     both the public URL and the upstream URL elide the default
///     `:443`; a truly-different port on the same host (e.g. a
///     side-channel Grafana on `:3000` while meshmon runs on `:443`)
///     is accepted since no recursion is possible.
fn reject_self_referential_upstream(
    raw_url: &str,
    listen_addr: &std::net::SocketAddr,
    public_base_url: Option<&str>,
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

    // Check 1: same host + same internal port → direct recursion.
    if let Some(port) = url.port_or_known_default() {
        if port == listen_addr.port() {
            let listen_hosts: &[&str] = match listen_addr.ip() {
                std::net::IpAddr::V4(v4) if v4.is_unspecified() => &["127.0.0.1", "localhost"],
                // IPv6 unspecified (`[::]`) is dual-stack on most OSes:
                // an IPv4 client connecting to `127.0.0.1:<port>` lands
                // on the same listener via v4-mapped-v6. Treat the v4
                // loopback as self too.
                std::net::IpAddr::V6(v6) if v6.is_unspecified() => {
                    &["::1", "127.0.0.1", "localhost"]
                }
                // Explicit loopback binds still recurse when the
                // operator writes the other loopback family alias or
                // `localhost` — the resolver translates them all to
                // the same local listener.
                std::net::IpAddr::V4(v4) if v4.is_loopback() => &["::1", "localhost"],
                std::net::IpAddr::V6(v6) if v6.is_loopback() => &["127.0.0.1", "localhost"],
                _ => &[],
            };
            let own_ip = listen_addr.ip().to_string();
            let mut matched = host == own_ip || listen_hosts.contains(&host);

            // Wildcard binds (`0.0.0.0` / `[::]`) also accept traffic
            // on every local interface IP. Reject an upstream URL
            // that targets any of them on the same port — a common
            // footgun when an operator inlines the machine's LAN IP.
            if !matched && listen_addr.ip().is_unspecified() {
                matched = host_matches_any_local_interface(host);
            }

            if matched {
                return Err(BootError::ConfigInvalid {
                    path: path.to_string(),
                    reason: format!(
                        "{key} `{raw_url}` recurses into meshmon's own listen address \
                         ({listen_addr}); set it to the upstream's internal address instead"
                    ),
                });
            }
        }
    }

    // Check 2: upstream URL points at meshmon's own public endpoint.
    // Behind nginx, public 443 + internal 8080 slips past check 1 but
    // still recurses (client → nginx → meshmon → /grafana/* → nginx
    // → meshmon → ...). Match on scheme-default-normalized
    // `host:port` rather than host alone so an operator legitimately
    // running a different service on the same hostname at a different
    // port (e.g. a side-channel Grafana on :3000 while meshmon is on
    // :443) is not rejected.
    if let Some(public) = public_base_url {
        if let Ok(pub_url) = url::Url::parse(public) {
            if let (Some(pub_host), Some(pub_port), Some(up_port)) = (
                pub_url.host_str(),
                pub_url.port_or_known_default(),
                url.port_or_known_default(),
            ) {
                if host == pub_host && up_port == pub_port {
                    return Err(BootError::ConfigInvalid {
                        path: path.to_string(),
                        reason: format!(
                            "{key} `{raw_url}` resolves to meshmon's public URL \
                             (`{public}`) and would recurse through the edge proxy; \
                             set it to the upstream's internal address instead"
                        ),
                    });
                }
            }
        }
    }

    Ok(())
}

/// Return `true` if `host` parses as a literal IP address that belongs
/// to one of this machine's local network interfaces. Used by the
/// self-referential upstream guard to catch the "wildcard bind +
/// inline interface IP" shape (`0.0.0.0` listener with
/// `grafana_url = "http://<LAN-IP>:<port>/..."`).
///
/// Returns `false` when `host` is not an IP literal (DNS names aren't
/// resolved), when `if_addrs::get_if_addrs` fails, or when there's no
/// match — callers should then fall through to the subsequent checks.
fn host_matches_any_local_interface(host: &str) -> bool {
    let Ok(target) = host.parse::<std::net::IpAddr>() else {
        return false;
    };
    match if_addrs::get_if_addrs() {
        Ok(interfaces) => interfaces.into_iter().any(|i| i.ip() == target),
        Err(_) => false,
    }
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

/// Resolve the `[enrichment.*]` block. Each provider toggle is passed
/// through verbatim; ipgeolocation is additionally gated on a terms-of-
/// service acknowledgement and on a present API key (inline or via
/// `api_key_env`).
///
/// The ToS and api_key validations only fire when
/// `ipgeolocation.enabled = true` so an operator who leaves the
/// provider disabled can keep the block in the file without setting
/// the env var.
fn resolve_enrichment(
    raw: RawEnrichmentSection,
    path: &str,
) -> Result<EnrichmentSection, BootError> {
    // ToS check first — a missing acknowledgement is the most
    // operator-actionable failure and shouldn't be masked by an
    // env-resolution error raised for a key we'd never use anyway.
    if raw.ipgeolocation.enabled && !raw.ipgeolocation.acknowledged_tos {
        return Err(BootError::ConfigInvalid {
            path: path.to_string(),
            reason: "[enrichment.ipgeolocation] enabled=true requires \
                     acknowledged_tos=true (see docs/campaigns/architecture.md \
                     — ipgeolocation ToS gate)"
                .to_string(),
        });
    }

    let ipgeolocation_api_key = if raw.ipgeolocation.enabled {
        // Resolve inline first, then env. When both are absent we
        // surface the specific "api_key or api_key_env" error below
        // rather than the generic `EnvMissing` so operators see the
        // exact knob they need to set.
        match (raw.ipgeolocation.api_key, raw.ipgeolocation.api_key_env) {
            (Some(v), _) if !v.is_empty() => Some(v),
            (_, Some(env)) => {
                resolve_optional_secret(None, Some(env), "enrichment.ipgeolocation.api_key", path)?
            }
            _ => None,
        }
    } else {
        // When disabled we don't touch env vars — an operator who
        // flipped enabled off should not need IPGEO_KEY defined.
        None
    };

    if raw.ipgeolocation.enabled && ipgeolocation_api_key.is_none() {
        return Err(BootError::ConfigInvalid {
            path: path.to_string(),
            reason: "[enrichment.ipgeolocation] enabled=true requires \
                     api_key or api_key_env"
                .to_string(),
        });
    }

    let ipgeolocation = IpGeolocationSection {
        enabled: raw.ipgeolocation.enabled,
        api_key: ipgeolocation_api_key,
        acknowledged_tos: raw.ipgeolocation.acknowledged_tos,
    };

    Ok(EnrichmentSection {
        ipgeolocation,
        rdap: RdapSection {
            enabled: raw.rdap.enabled,
        },
        maxmind: MaxmindSection {
            enabled: raw.maxmind.enabled,
            city_mmdb: raw.maxmind.city_mmdb,
            asn_mmdb: raw.maxmind.asn_mmdb,
        },
        whois: WhoisSection {
            enabled: raw.whois.enabled,
        },
    })
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

    let (queue, _rx) = crate::enrichment::runner::EnrichmentQueue::new(1024);
    AppState::new(
        swap,
        rx,
        pool,
        ingestion,
        registry,
        crate::metrics::test_install(),
        Arc::new(queue),
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
    fn upstream_pointing_at_public_url_is_rejected_even_on_different_port() {
        // Common nginx deployment: internal listener on 8080, public
        // URL on 443. An operator who pastes the public URL into
        // `grafana_url` still recurses (through nginx back into
        // meshmon), but check-1 (exact port equality) would miss it.
        let toml = format!(
            r#"{MIN_TOML}
[service]
listen_addr = "127.0.0.1:8080"
public_base_url = "https://meshmon.example.com"

[upstream]
grafana_url = "https://meshmon.example.com/grafana"
"#
        );
        let err = Config::from_str(&toml, "t.toml").expect_err("must reject");
        match err {
            BootError::ConfigInvalid { reason, .. } => {
                assert!(
                    reason.contains("public URL") || reason.contains("recurs"),
                    "expected recursion-guard message, got: {reason}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn upstream_on_public_host_but_different_port_is_accepted() {
        // Valid deployment: meshmon is on :443 via the edge proxy, and
        // a side-channel Grafana is exposed on :3000 at the same
        // hostname (it doesn't come back through meshmon). The guard
        // should accept this — rejecting would block legitimate
        // multi-service-on-one-hostname topologies.
        let toml = format!(
            r#"{MIN_TOML}
[service]
listen_addr = "127.0.0.1:8080"
public_base_url = "https://meshmon.example.com"

[upstream]
grafana_url = "https://meshmon.example.com:3000/grafana"
"#
        );
        let cfg = Config::from_str(&toml, "t.toml").expect("must accept");
        assert_eq!(
            cfg.upstream.grafana_url.as_deref(),
            Some("https://meshmon.example.com:3000/grafana"),
        );
    }

    #[test]
    fn upstream_on_local_interface_ip_is_rejected_for_wildcard_bind() {
        // `0.0.0.0:<port>` accepts on every interface, so an upstream
        // URL that targets a specific local interface IP at the same
        // port still recurses. Find any non-loopback interface IP on
        // this host and assert the guard rejects it.
        // Pick the first non-loopback *IPv4* interface IP. IPv6
        // link-locals (`fe80::.../%iface`) carry zone identifiers
        // that don't survive URL round-tripping, so they're awkward
        // to build a test URL from — IPv4 is enough to exercise the
        // enumeration path.
        let Some(interface_ip) = if_addrs::get_if_addrs()
            .ok()
            .into_iter()
            .flatten()
            .map(|i| i.ip())
            .find(|ip| matches!(ip, std::net::IpAddr::V4(v4) if !v4.is_loopback()))
            .map(|ip| ip.to_string())
        else {
            // No suitable interface (e.g. sandboxed CI env); skip
            // rather than false-negative.
            eprintln!("skipping: no non-loopback IPv4 interface available");
            return;
        };

        let toml = format!(
            r#"{MIN_TOML}
[service]
listen_addr = "0.0.0.0:8080"

[upstream]
grafana_url = "http://{interface_ip}:8080/grafana"
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
                    reason.contains("visible ASCII"),
                    "expected visible-ASCII message, got: {reason}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn username_with_non_ascii_is_rejected() {
        // `HeaderValue::try_from(&str)` rejects bytes >= 0x80, so a
        // username like "mário" would pass the control-byte filter
        // but then panic on the first Grafana request when the proxy
        // tries to build `X-WEBAUTH-USER`. Reject at load.
        let valid_hash =
            "$argon2id$v=19$m=16,t=1,p=1$c2FsdHNhbHQ$87ARSxtFrFp/0EGLYgzI7Giyu6y7PD1rUqoZugn3NqY";
        let toml = format!(
            "{MIN_TOML}\n[[auth.users]]\nusername = \"mário\"\npassword_hash = \"{valid_hash}\"\n"
        );
        let err = Config::from_str(&toml, "t.toml").expect_err("must reject");
        match err {
            BootError::ConfigInvalid { reason, .. } => {
                assert!(
                    reason.contains("visible ASCII"),
                    "expected visible-ASCII message, got: {reason}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    const VALID_PHC: &str =
        "$argon2id$v=19$m=16,t=1,p=1$c2FsdHNhbHQ$87ARSxtFrFp/0EGLYgzI7Giyu6y7PD1rUqoZugn3NqY";

    #[test]
    fn auth_user_password_hash_resolves_from_env_indirection() {
        std::env::set_var("MESHMON_TEST_ADMIN_HASH_XYZ", VALID_PHC);
        let toml = format!(
            "{MIN_TOML}\n[[auth.users]]\nusername = \"admin\"\npassword_hash_env = \"MESHMON_TEST_ADMIN_HASH_XYZ\"\n"
        );
        let cfg = Config::from_str(&toml, "t.toml").expect("parse");
        std::env::remove_var("MESHMON_TEST_ADMIN_HASH_XYZ");
        assert_eq!(cfg.auth.users.len(), 1);
        assert_eq!(cfg.auth.users[0].username, "admin");
        assert_eq!(cfg.auth.users[0].password_hash, VALID_PHC);
    }

    #[test]
    fn auth_user_username_resolves_from_env_indirection() {
        std::env::set_var("MESHMON_TEST_ADMIN_NAME_XYZ", "alice");
        let toml = format!(
            "{MIN_TOML}\n[[auth.users]]\nusername_env = \"MESHMON_TEST_ADMIN_NAME_XYZ\"\npassword_hash = \"{VALID_PHC}\"\n"
        );
        let cfg = Config::from_str(&toml, "t.toml").expect("parse");
        std::env::remove_var("MESHMON_TEST_ADMIN_NAME_XYZ");
        assert_eq!(cfg.auth.users[0].username, "alice");
    }

    #[test]
    fn auth_user_rejects_both_inline_and_env_hash() {
        let toml = format!(
            "{MIN_TOML}\n[[auth.users]]\nusername = \"admin\"\npassword_hash = \"{VALID_PHC}\"\npassword_hash_env = \"MESHMON_UNSET_XYZ\"\n"
        );
        let err = Config::from_str(&toml, "t.toml").expect_err("must reject");
        match err {
            BootError::ConfigInvalid { reason, .. } => {
                assert!(
                    reason.contains("auth.users[0]") && reason.contains("both"),
                    "unexpected reason: {reason}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn auth_user_rejects_neither_inline_nor_env_hash() {
        let toml = format!("{MIN_TOML}\n[[auth.users]]\nusername = \"admin\"\n");
        let err = Config::from_str(&toml, "t.toml").expect_err("must reject");
        match err {
            BootError::ConfigInvalid { reason, .. } => {
                assert!(
                    reason.contains("auth.users[0]") && reason.contains("password_hash"),
                    "unexpected reason: {reason}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn auth_user_rejects_empty_username_env() {
        std::env::set_var("MESHMON_TEST_EMPTY_USERNAME", "");
        let toml = format!(
            "{MIN_TOML}\n[[auth.users]]\nusername_env = \"MESHMON_TEST_EMPTY_USERNAME\"\npassword_hash = \"{VALID_PHC}\"\n"
        );
        let err = Config::from_str(&toml, "t.toml").expect_err("must reject");
        std::env::remove_var("MESHMON_TEST_EMPTY_USERNAME");
        match err {
            BootError::ConfigInvalid { reason, .. } => {
                assert!(
                    reason.contains("auth.users[0]") && reason.contains("username"),
                    "unexpected reason: {reason}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn refuses_ipgeolocation_without_tos_ack() {
        // Enabling the ipgeolocation provider without the ToS flag
        // must fail at load — the provider's free tier makes the
        // acknowledgement a launch-gate, not a runtime nudge.
        let toml = format!(
            r#"{MIN_TOML}
[enrichment.ipgeolocation]
enabled = true
api_key_env = "MESHMON_TEST_IPGEO_KEY_FOR_TOS_TEST"
acknowledged_tos = false
"#
        );
        let err = Config::from_str(&toml, "t.toml").expect_err("must reject");
        assert!(
            err.to_string().to_lowercase().contains("acknowledged_tos"),
            "expected acknowledged_tos mention, got: {err}"
        );
    }

    #[test]
    fn accepts_ipgeolocation_disabled() {
        // When the provider is disabled the ToS and api_key checks
        // never fire, so the block parses without needing the env
        // var to be set.
        let toml = format!(
            r#"{MIN_TOML}
[enrichment.ipgeolocation]
enabled = false
api_key_env = "MESHMON_TEST_IPGEO_KEY_UNSET"
acknowledged_tos = false
"#
        );
        let cfg = Config::from_str(&toml, "t.toml").expect("must accept");
        assert!(!cfg.enrichment.ipgeolocation.enabled);
        assert!(cfg.enrichment.ipgeolocation.api_key.is_none());
        assert!(!cfg.enrichment.ipgeolocation.acknowledged_tos);
    }

    #[test]
    fn refuses_ipgeolocation_without_api_key() {
        // Enabled + ToS ack but no API key (neither inline nor env)
        // is a launch-time misconfiguration — catch it at load.
        let toml = format!(
            r#"{MIN_TOML}
[enrichment.ipgeolocation]
enabled = true
acknowledged_tos = true
"#
        );
        let err = Config::from_str(&toml, "t.toml").expect_err("must reject");
        match err {
            BootError::ConfigInvalid { reason, .. } => {
                assert!(
                    reason.contains("api_key"),
                    "expected api_key mention, got: {reason}"
                );
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn accepts_ipgeolocation_enabled_with_env_key() {
        // Full valid setup: enabled, ToS acked, and the env var
        // actually populated. Should load cleanly and the key
        // should be threaded through to the resolved section.
        std::env::set_var("MESHMON_TEST_IPGEO_KEY_VALID", "sekret-key-123");
        let toml = format!(
            r#"{MIN_TOML}
[enrichment.ipgeolocation]
enabled = true
api_key_env = "MESHMON_TEST_IPGEO_KEY_VALID"
acknowledged_tos = true
"#
        );
        let cfg = Config::from_str(&toml, "t.toml").expect("must accept");
        std::env::remove_var("MESHMON_TEST_IPGEO_KEY_VALID");
        assert!(cfg.enrichment.ipgeolocation.enabled);
        assert_eq!(
            cfg.enrichment.ipgeolocation.api_key.as_deref(),
            Some("sekret-key-123"),
        );
        assert!(cfg.enrichment.ipgeolocation.acknowledged_tos);
    }

    #[test]
    fn enrichment_defaults_when_section_absent() {
        // No [enrichment] block at all → paid / feature-gated providers
        // stay disabled, and RDAP opts in by default (see
        // `rdap_enabled_default` for the reasoning).
        let cfg = Config::from_str(MIN_TOML, "t.toml").expect("parse");
        assert!(!cfg.enrichment.ipgeolocation.enabled);
        assert!(cfg.enrichment.rdap.enabled);
        assert!(!cfg.enrichment.maxmind.enabled);
        assert!(!cfg.enrichment.whois.enabled);
    }

    #[test]
    fn campaigns_section_defaults_and_overrides() {
        let toml = format!(
            r#"{MIN_TOML}
[campaigns]
enabled = true
size_warning_threshold = 500
scheduler_tick_ms = 250
max_pair_attempts = 5
per_destination_rps = 4
"#
        );
        let cfg = Config::from_str(&toml, "t.toml").expect("parse");
        assert!(cfg.campaigns.enabled);
        assert_eq!(cfg.campaigns.size_warning_threshold, 500);
        assert_eq!(cfg.campaigns.scheduler_tick_ms, 250);
        assert_eq!(cfg.campaigns.max_pair_attempts, 5);
        assert_eq!(cfg.campaigns.per_destination_rps, 4);

        // Defaults apply when the section is omitted. Scheduler ships
        // disabled by default — a real prober lands in T46; until then
        // the agent's `StubProber` would persist synthetic measurements.
        let cfg = Config::from_str(MIN_TOML, "t.toml").expect("parse");
        assert!(
            !cfg.campaigns.enabled,
            "scheduler must default to disabled until T46 ships a real prober",
        );
        assert_eq!(cfg.campaigns.size_warning_threshold, 1000);
        assert_eq!(cfg.campaigns.scheduler_tick_ms, 500);
        assert_eq!(cfg.campaigns.max_pair_attempts, 3);
        assert_eq!(cfg.campaigns.per_destination_rps, 2);
    }

    #[test]
    fn campaigns_rejects_zero_tick() {
        let toml = format!(
            r#"{MIN_TOML}
[campaigns]
scheduler_tick_ms = 0
"#
        );
        let err = Config::from_str(&toml, "t.toml").unwrap_err().to_string();
        assert!(err.contains("scheduler_tick_ms"), "err = {err}");
    }

    #[test]
    fn campaigns_defaults_disabled_with_dispatch_knobs_seeded() {
        // Scheduler stays off by default so the agent's `StubProber`
        // does not persist synthetic campaign measurements. Dispatch
        // knobs (`default_agent_concurrency`, `max_batch_size`) still
        // seed with production values so T46 only has to flip
        // `enabled = true` — no other config surgery required.
        let cfg = Config::from_str(MIN_TOML, "t.toml").expect("parse");
        assert!(
            !cfg.campaigns.enabled,
            "scheduler must default off until a real prober ships",
        );
        assert_eq!(cfg.campaigns.default_agent_concurrency, 16);
        assert_eq!(cfg.campaigns.max_batch_size, 50);
    }

    #[test]
    fn campaigns_overrides_respect_ops_settings() {
        let toml = format!(
            r#"{MIN_TOML}
[campaigns]
enabled = false
default_agent_concurrency = 8
max_batch_size = 20
"#
        );
        let cfg = Config::from_str(&toml, "t.toml").expect("parse");
        assert!(!cfg.campaigns.enabled);
        assert_eq!(cfg.campaigns.default_agent_concurrency, 8);
        assert_eq!(cfg.campaigns.max_batch_size, 20);
    }

    #[test]
    fn campaigns_rejects_zero_agent_concurrency() {
        let toml = format!(
            r#"{MIN_TOML}
[campaigns]
default_agent_concurrency = 0
"#
        );
        let err = Config::from_str(&toml, "t.toml").unwrap_err().to_string();
        assert!(err.contains("default_agent_concurrency"), "err = {err}");
    }

    #[test]
    fn campaigns_rejects_zero_batch_size() {
        let toml = format!(
            r#"{MIN_TOML}
[campaigns]
max_batch_size = 0
"#
        );
        let err = Config::from_str(&toml, "t.toml").unwrap_err().to_string();
        assert!(err.contains("max_batch_size"), "err = {err}");
    }
}

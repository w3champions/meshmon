//! Environment-based configuration for the meshmon agent.
//!
//! [`AgentEnv`] reads required environment variables at startup and validates
//! them eagerly, reporting *all* missing or invalid values at once so the
//! operator can fix everything in a single pass.

use std::net::IpAddr;

use anyhow::{bail, Result};
use meshmon_protocol::{
    DiffDetection, PathHealth as PbPathHealth, PathHealthThresholds, Protocol, ProtocolThresholds,
    RateEntry,
};

// ---------------------------------------------------------------------------
// Agent identity
// ---------------------------------------------------------------------------

/// Static identity of this agent, derived from environment variables set at
/// deploy time.
#[derive(Debug, Clone)]
pub struct AgentIdentity {
    /// Unique machine-readable name, e.g. `"brazil-north"`.
    pub id: String,
    /// Human-friendly label, e.g. `"Brazil North"`.
    pub display_name: String,
    /// Free-form location string, e.g. `"Fortaleza, Brazil"`.
    pub location: String,
    /// The agent's externally-reachable IP.
    pub ip: IpAddr,
    /// Latitude in decimal degrees (-90..90).
    pub lat: f64,
    /// Longitude in decimal degrees (-180..180).
    pub lon: f64,
}

// ---------------------------------------------------------------------------
// Environment bundle
// ---------------------------------------------------------------------------

/// All environment variables required by the agent, validated at startup.
#[derive(Debug, Clone)]
pub struct AgentEnv {
    /// gRPC endpoint of the central meshmon service, e.g.
    /// `"https://meshmon.example.com:443"`.
    pub service_url: String,
    /// Bearer token for authenticating with the service.
    pub agent_token: String,
    /// The agent's static identity.
    pub identity: AgentIdentity,
    /// Semantic version of the agent binary (compiled in).
    pub agent_version: String,
    /// Port the TCP echo listener binds to (and peers probe us on).
    pub tcp_probe_port: u16,
    /// Port the UDP echo listener binds to (and peers probe us on).
    pub udp_probe_port: u16,
    /// Global cap on concurrent per-target ICMP/traceroute rounds.
    /// Defaults to 32 when unset.
    pub icmp_target_concurrency: usize,
    /// Optional per-agent override for the cluster-wide campaign
    /// concurrency cap. Read from `MESHMON_CAMPAIGN_MAX_CONCURRENCY`;
    /// `None` means "follow the cluster default" (persisted unset on the
    /// service side so the dispatcher falls back to
    /// `[campaigns.default_agent_concurrency]`). Zero is rejected at
    /// parse time because it would permanently block the agent from
    /// receiving campaign batches.
    pub campaign_max_concurrency: Option<u32>,
}

/// Read a required env var, pushing an error message if missing.
fn read_required(name: &str, errors: &mut Vec<String>) -> Option<String> {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => {
            errors.push(format!("{name}: required but not set"));
            None
        }
    }
}

/// Read and parse a required `u16` port env var. Rejects `0` and values that
/// cannot be parsed as `u16`. Returns `None` on failure (error pushed).
fn parse_port_required(name: &str, errors: &mut Vec<String>) -> Option<u16> {
    let raw = read_required(name, errors)?;
    match raw.parse::<u16>() {
        Ok(0) => {
            errors.push(format!("{name}: must not be zero"));
            None
        }
        Ok(n) => Some(n),
        Err(e) => {
            errors.push(format!("{name}: invalid port {raw:?}: {e}"));
            None
        }
    }
}

impl AgentEnv {
    /// Read and validate all required environment variables.
    ///
    /// Collects *every* error before bailing so the operator sees all problems
    /// at once rather than playing whack-a-mole.
    pub fn from_env() -> Result<Self> {
        let mut errors = Vec::new();

        // -- plain string vars (no parsing needed) --
        let service_url = read_required("MESHMON_SERVICE_URL", &mut errors);
        let agent_token = read_required("MESHMON_AGENT_TOKEN", &mut errors);
        let id = read_required("AGENT_ID", &mut errors);
        let display_name = read_required("AGENT_DISPLAY_NAME", &mut errors);
        let location = read_required("AGENT_LOCATION", &mut errors);
        let agent_version = env!("CARGO_PKG_VERSION").to_string();

        // -- IP address --
        let ip_raw = read_required("AGENT_IP", &mut errors);
        let ip = ip_raw.and_then(|raw| match raw.parse::<IpAddr>() {
            Ok(addr) => Some(addr),
            Err(e) => {
                errors.push(format!("AGENT_IP: invalid IP address {raw:?}: {e}"));
                None
            }
        });

        // -- latitude --
        let lat_raw = read_required("AGENT_LAT", &mut errors);
        let lat = lat_raw.and_then(|raw| match raw.parse::<f64>() {
            Ok(v) if (-90.0..=90.0).contains(&v) => Some(v),
            Ok(v) => {
                errors.push(format!("AGENT_LAT: value {v} out of range (-90..90)"));
                None
            }
            Err(e) => {
                errors.push(format!("AGENT_LAT: invalid float {raw:?}: {e}"));
                None
            }
        });

        // -- longitude --
        let lon_raw = read_required("AGENT_LON", &mut errors);
        let lon = lon_raw.and_then(|raw| match raw.parse::<f64>() {
            Ok(v) if (-180.0..=180.0).contains(&v) => Some(v),
            Ok(v) => {
                errors.push(format!("AGENT_LON: value {v} out of range (-180..180)"));
                None
            }
            Err(e) => {
                errors.push(format!("AGENT_LON: invalid float {raw:?}: {e}"));
                None
            }
        });

        // -- probe ports --
        let tcp_probe_port = parse_port_required("MESHMON_TCP_PROBE_PORT", &mut errors);
        let udp_probe_port = parse_port_required("MESHMON_UDP_PROBE_PORT", &mut errors);

        // -- per-target ICMP/traceroute concurrency (optional, default 32) --
        let icmp_target_concurrency = match std::env::var("MESHMON_ICMP_TARGET_CONCURRENCY") {
            Err(_) => 32,
            Ok(raw) => match raw.parse::<usize>() {
                Ok(n) if (1..=1024).contains(&n) => n,
                Ok(n) => {
                    errors.push(format!(
                        "MESHMON_ICMP_TARGET_CONCURRENCY: {n} out of range [1, 1024]"
                    ));
                    0
                }
                Err(e) => {
                    errors.push(format!(
                        "MESHMON_ICMP_TARGET_CONCURRENCY: invalid usize {raw:?}: {e}"
                    ));
                    0
                }
            },
        };

        // -- campaign concurrency override (optional, None = cluster default) --
        //
        // Parse failures are pushed as errors rather than silently
        // discarded so operators notice typos. A zero value is rejected
        // because it would permanently block the agent from receiving
        // campaign batches.
        let campaign_max_concurrency = match std::env::var("MESHMON_CAMPAIGN_MAX_CONCURRENCY") {
            Err(_) => None,
            Ok(raw) => match raw.parse::<u32>() {
                Ok(0) => {
                    errors.push(
                        "MESHMON_CAMPAIGN_MAX_CONCURRENCY: must be >= 1 (0 would block all batches)"
                            .to_string(),
                    );
                    None
                }
                Ok(n) => Some(n),
                Err(e) => {
                    errors.push(format!(
                        "MESHMON_CAMPAIGN_MAX_CONCURRENCY: invalid u32 {raw:?}: {e}"
                    ));
                    None
                }
            },
        };

        // -- report all collected errors --
        if !errors.is_empty() {
            bail!("invalid agent environment:\n  {}", errors.join("\n  "));
        }

        // All Options are Some when errors is empty.
        Ok(Self {
            service_url: service_url.unwrap(),
            agent_token: agent_token.unwrap(),
            identity: AgentIdentity {
                id: id.unwrap(),
                display_name: display_name.unwrap(),
                location: location.unwrap(),
                ip: ip.unwrap(),
                lat: lat.unwrap(),
                lon: lon.unwrap(),
            },
            agent_version,
            tcp_probe_port: tcp_probe_port.unwrap(),
            udp_probe_port: udp_probe_port.unwrap(),
            icmp_target_concurrency,
            campaign_max_concurrency,
        })
    }
}

// ---------------------------------------------------------------------------
// Probe configuration (from service)
// ---------------------------------------------------------------------------

/// Probe configuration received from the service via `GetConfig`.
///
/// This is a Rust-native view of the protobuf `ConfigResponse`. Downstream
/// tasks (T12–T14) read fields from this struct to configure probers and
/// state machines; T11 only stores and broadcasts it.
#[derive(Debug, Clone)]
pub struct ProbeConfig {
    /// Raw proto response — downstream tasks extract what they need.
    pub(crate) raw: meshmon_protocol::ConfigResponse,
    /// Current UDP probe secret (exactly 8 bytes).
    pub(crate) udp_probe_secret: [u8; 8],
    /// Previous UDP probe secret during rotation; `None` when absent.
    pub(crate) udp_probe_previous_secret: Option<[u8; 8]>,
    /// Window size (seconds) used by `RollingStats` for the protocol
    /// currently primary on a path. Spec 02 default: 300. Consumed by the
    /// supervisor's primary-swing `set_window` call on every eval tick.
    pub(crate) primary_window_sec: u32,
    /// Window size (seconds) for non-primary (diversity) protocols.
    /// Spec 02 default: 900.
    pub(crate) diversity_window_sec: u32,
}

impl ProbeConfig {
    /// Wrap a `ConfigResponse` received from the service, validating that the
    /// UDP probe secrets are the expected 8 bytes when present.
    pub fn from_proto(resp: meshmon_protocol::ConfigResponse) -> Result<Self> {
        let udp_probe_secret = to_fixed_secret(&resp.udp_probe_secret, "udp_probe_secret")?;
        let udp_probe_previous_secret = if resp.udp_probe_previous_secret.is_empty() {
            None
        } else {
            Some(to_fixed_secret(
                &resp.udp_probe_previous_secret,
                "udp_probe_previous_secret",
            )?)
        };
        // Defaults match spec 02 § Window size by role. The service is
        // authoritative here (its parser enforces > 0); we only fall back
        // to defaults if the field is missing entirely (e.g. an old
        // service or a deliberately partial test fixture).
        let (primary_window_sec, diversity_window_sec) = match resp.windows.as_ref() {
            Some(w) => (w.primary_sec, w.diversity_sec),
            None => (300, 900),
        };
        Ok(Self {
            raw: resp,
            udp_probe_secret,
            udp_probe_previous_secret,
            primary_window_sec,
            diversity_window_sec,
        })
    }

    pub fn priority_list(&self) -> Vec<Protocol> {
        let list: Vec<Protocol> = self
            .raw
            .priority
            .iter()
            .filter_map(|n| Protocol::try_from(*n).ok())
            .filter(|p| *p != Protocol::Unspecified)
            .collect();
        if list.is_empty() {
            vec![Protocol::Icmp, Protocol::Tcp, Protocol::Udp]
        } else {
            list
        }
    }

    pub fn thresholds_for(&self, protocol: Protocol) -> ProtocolThresholds {
        match protocol {
            Protocol::Icmp => self
                .raw
                .icmp_thresholds
                .unwrap_or_else(default_icmp_thresholds),
            Protocol::Tcp => self
                .raw
                .tcp_thresholds
                .unwrap_or_else(default_tcp_thresholds),
            Protocol::Udp => self
                .raw
                .udp_thresholds
                .unwrap_or_else(default_udp_thresholds),
            Protocol::Unspecified => default_icmp_thresholds(),
        }
    }

    pub fn path_thresholds(&self) -> PathHealthThresholds {
        self.raw
            .path_health_thresholds
            .unwrap_or_else(default_path_thresholds)
    }

    /// Route-change detection thresholds. Falls back to spec 02 defaults
    /// when the service hasn't supplied a `DiffDetection` message.
    pub fn diff_detection(&self) -> DiffDetection {
        self.raw
            .diff_detection
            .unwrap_or_else(default_diff_detection)
    }

    pub fn rates_for(&self, primary: Protocol, health: PbPathHealth) -> Option<RateEntry> {
        self.raw
            .rates
            .iter()
            .find(|r| {
                Protocol::try_from(r.primary).ok() == Some(primary)
                    && PbPathHealth::try_from(r.health).ok() == Some(health)
            })
            .cloned()
    }
}

fn default_icmp_thresholds() -> ProtocolThresholds {
    ProtocolThresholds {
        unhealthy_trigger_pct: 0.9,
        healthy_recovery_pct: 0.1,
        unhealthy_hysteresis_sec: 30,
        healthy_hysteresis_sec: 60,
    }
}

fn default_tcp_thresholds() -> ProtocolThresholds {
    ProtocolThresholds {
        unhealthy_trigger_pct: 0.5,
        healthy_recovery_pct: 0.05,
        unhealthy_hysteresis_sec: 30,
        healthy_hysteresis_sec: 60,
    }
}

fn default_udp_thresholds() -> ProtocolThresholds {
    ProtocolThresholds {
        unhealthy_trigger_pct: 0.9,
        healthy_recovery_pct: 0.1,
        unhealthy_hysteresis_sec: 30,
        healthy_hysteresis_sec: 60,
    }
}

fn default_path_thresholds() -> PathHealthThresholds {
    PathHealthThresholds {
        degraded_trigger_pct: 0.05,
        degraded_trigger_sec: 120,
        degraded_min_samples: 30,
        normal_recovery_pct: 0.02,
        normal_recovery_sec: 300,
    }
}

fn default_diff_detection() -> DiffDetection {
    DiffDetection {
        new_ip_min_freq: 0.20,
        hop_count_change: 1,
    }
}

/// Copy an exactly-8-byte field from a `Bytes`-style slice into a fixed array,
/// returning a descriptive error if the length is wrong.
fn to_fixed_secret(bytes: &[u8], field: &str) -> Result<[u8; 8]> {
    if bytes.len() != 8 {
        bail!("{field}: expected 8 bytes, got {}", bytes.len());
    }
    let mut out = [0u8; 8];
    out.copy_from_slice(bytes);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::Mutex;

    /// Serialize tests that mutate process-global environment variables.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Required env var names touched by the agent config. Every entry here
    /// must produce an error when missing.
    const REQUIRED_VARS: [&str; 10] = [
        "MESHMON_SERVICE_URL",
        "MESHMON_AGENT_TOKEN",
        "AGENT_ID",
        "AGENT_DISPLAY_NAME",
        "AGENT_LOCATION",
        "AGENT_IP",
        "AGENT_LAT",
        "AGENT_LON",
        "MESHMON_TCP_PROBE_PORT",
        "MESHMON_UDP_PROBE_PORT",
    ];

    /// Optional env vars the agent accepts. Cleared between tests to prevent
    /// leakage but not required for `from_env` to succeed.
    const OPTIONAL_VARS: [&str; 2] = [
        "MESHMON_ICMP_TARGET_CONCURRENCY",
        "MESHMON_CAMPAIGN_MAX_CONCURRENCY",
    ];

    /// Clear every env var this test module is aware of, both required and
    /// optional, to prevent cross-test leakage.
    fn clear_all_env_vars() {
        for k in &REQUIRED_VARS {
            env::remove_var(k);
        }
        for k in &OPTIONAL_VARS {
            env::remove_var(k);
        }
    }

    /// Helper: hold the env lock, clear all agent vars, set the valid
    /// defaults, run the closure, then clean up. Ensures no env leakage
    /// between parallel tests.
    fn with_valid_env(f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_env_vars();
        let vars = [
            ("MESHMON_SERVICE_URL", "https://meshmon.example.com:443"),
            ("MESHMON_AGENT_TOKEN", "test-token-123"),
            ("AGENT_ID", "brazil-north"),
            ("AGENT_DISPLAY_NAME", "Brazil North"),
            ("AGENT_LOCATION", "Fortaleza, Brazil"),
            ("AGENT_IP", "170.80.110.90"),
            ("AGENT_LAT", "-3.7172"),
            ("AGENT_LON", "-38.5433"),
            ("MESHMON_TCP_PROBE_PORT", "3555"),
            ("MESHMON_UDP_PROBE_PORT", "3552"),
        ];
        for (k, v) in &vars {
            env::set_var(k, v);
        }
        f();
        clear_all_env_vars();
    }

    /// Helper: hold the env lock, clear all agent vars, run the closure,
    /// then clean up.
    fn with_cleared_env(f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_all_env_vars();
        f();
    }

    #[test]
    fn parses_valid_env() {
        with_valid_env(|| {
            let env = AgentEnv::from_env().expect("should parse valid env");
            assert_eq!(env.service_url, "https://meshmon.example.com:443");
            assert_eq!(env.agent_token, "test-token-123");
            assert_eq!(env.identity.id, "brazil-north");
            assert_eq!(env.identity.display_name, "Brazil North");
            assert_eq!(env.identity.location, "Fortaleza, Brazil");
            assert_eq!(env.identity.ip, "170.80.110.90".parse::<IpAddr>().unwrap());
            assert!(
                (env.identity.lat - (-3.7172)).abs() < 1e-9,
                "lat mismatch: {}",
                env.identity.lat,
            );
            assert!(
                (env.identity.lon - (-38.5433)).abs() < 1e-9,
                "lon mismatch: {}",
                env.identity.lon,
            );
        });
    }

    #[test]
    fn rejects_missing_vars() {
        with_cleared_env(|| {
            let err = AgentEnv::from_env().unwrap_err();
            let msg = err.to_string();
            for var in &REQUIRED_VARS {
                assert!(
                    msg.contains(var),
                    "error should mention missing var {var}, got: {msg}"
                );
            }
        });
    }

    #[test]
    fn rejects_invalid_ip() {
        with_valid_env(|| {
            env::set_var("AGENT_IP", "not-an-ip");
            let err = AgentEnv::from_env().unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("AGENT_IP"),
                "error should mention AGENT_IP, got: {msg}"
            );
        });
    }

    #[test]
    fn rejects_invalid_lat() {
        with_valid_env(|| {
            env::set_var("AGENT_LAT", "not-a-number");
            let err = AgentEnv::from_env().unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("AGENT_LAT"),
                "error should mention AGENT_LAT, got: {msg}"
            );
        });
    }

    #[test]
    fn rejects_out_of_range_lat() {
        with_valid_env(|| {
            env::set_var("AGENT_LAT", "91.0");
            let err = AgentEnv::from_env().unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("AGENT_LAT"),
                "error should mention AGENT_LAT for out-of-range, got: {msg}"
            );
        });
    }

    #[test]
    fn rejects_out_of_range_lon() {
        with_valid_env(|| {
            env::set_var("AGENT_LON", "181.0");
            let err = AgentEnv::from_env().unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("AGENT_LON"),
                "error should mention AGENT_LON for out-of-range, got: {msg}"
            );
        });
    }

    #[test]
    fn parses_probe_ports() {
        with_valid_env(|| {
            env::set_var("MESHMON_TCP_PROBE_PORT", "3555");
            env::set_var("MESHMON_UDP_PROBE_PORT", "3552");
            let env = AgentEnv::from_env().expect("valid");
            assert_eq!(env.tcp_probe_port, 3555);
            assert_eq!(env.udp_probe_port, 3552);
            assert_eq!(env.icmp_target_concurrency, 32);
        });
    }

    #[test]
    fn rejects_zero_tcp_probe_port() {
        with_valid_env(|| {
            env::set_var("MESHMON_TCP_PROBE_PORT", "0");
            env::set_var("MESHMON_UDP_PROBE_PORT", "3552");
            let err = AgentEnv::from_env().unwrap_err();
            assert!(err.to_string().contains("MESHMON_TCP_PROBE_PORT"), "{err}");
        });
    }

    #[test]
    fn rejects_invalid_tcp_probe_port() {
        with_valid_env(|| {
            env::set_var("MESHMON_TCP_PROBE_PORT", "not-a-port");
            let err = AgentEnv::from_env().unwrap_err();
            assert!(err.to_string().contains("MESHMON_TCP_PROBE_PORT"), "{err}");
        });
    }

    #[test]
    fn icmp_target_concurrency_override() {
        with_valid_env(|| {
            env::set_var("MESHMON_TCP_PROBE_PORT", "3555");
            env::set_var("MESHMON_UDP_PROBE_PORT", "3552");
            env::set_var("MESHMON_ICMP_TARGET_CONCURRENCY", "8");
            let env = AgentEnv::from_env().unwrap();
            assert_eq!(env.icmp_target_concurrency, 8);
        });
    }

    #[test]
    fn rejects_icmp_target_concurrency_out_of_range() {
        with_valid_env(|| {
            env::set_var("MESHMON_ICMP_TARGET_CONCURRENCY", "2048");
            let err = AgentEnv::from_env().unwrap_err();
            assert!(
                err.to_string().contains("MESHMON_ICMP_TARGET_CONCURRENCY"),
                "{err}"
            );
        });
    }

    #[test]
    fn rejects_icmp_target_concurrency_zero() {
        with_valid_env(|| {
            env::set_var("MESHMON_ICMP_TARGET_CONCURRENCY", "0");
            let err = AgentEnv::from_env().unwrap_err();
            assert!(
                err.to_string().contains("MESHMON_ICMP_TARGET_CONCURRENCY"),
                "{err}"
            );
        });
    }

    #[test]
    fn rejects_icmp_target_concurrency_invalid() {
        with_valid_env(|| {
            env::set_var("MESHMON_ICMP_TARGET_CONCURRENCY", "not-a-number");
            let err = AgentEnv::from_env().unwrap_err();
            assert!(
                err.to_string().contains("MESHMON_ICMP_TARGET_CONCURRENCY"),
                "{err}"
            );
        });
    }

    #[test]
    fn campaign_max_concurrency_defaults_to_none() {
        with_valid_env(|| {
            let env = AgentEnv::from_env().expect("valid");
            assert_eq!(env.campaign_max_concurrency, None);
        });
    }

    #[test]
    fn campaign_max_concurrency_override_is_parsed() {
        with_valid_env(|| {
            env::set_var("MESHMON_CAMPAIGN_MAX_CONCURRENCY", "8");
            let env = AgentEnv::from_env().expect("valid");
            assert_eq!(env.campaign_max_concurrency, Some(8));
        });
    }

    #[test]
    fn rejects_campaign_max_concurrency_zero() {
        with_valid_env(|| {
            env::set_var("MESHMON_CAMPAIGN_MAX_CONCURRENCY", "0");
            let err = AgentEnv::from_env().unwrap_err();
            assert!(
                err.to_string().contains("MESHMON_CAMPAIGN_MAX_CONCURRENCY"),
                "{err}"
            );
        });
    }

    #[test]
    fn rejects_campaign_max_concurrency_invalid() {
        with_valid_env(|| {
            env::set_var("MESHMON_CAMPAIGN_MAX_CONCURRENCY", "not-a-number");
            let err = AgentEnv::from_env().unwrap_err();
            assert!(
                err.to_string().contains("MESHMON_CAMPAIGN_MAX_CONCURRENCY"),
                "{err}"
            );
        });
    }

    #[test]
    fn probe_config_extracts_secret() {
        let resp = meshmon_protocol::ConfigResponse {
            udp_probe_secret: vec![1, 2, 3, 4, 5, 6, 7, 8].into(),
            ..Default::default()
        };
        let cfg = ProbeConfig::from_proto(resp).expect("valid");
        assert_eq!(cfg.udp_probe_secret, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert!(cfg.udp_probe_previous_secret.is_none());
    }

    #[test]
    fn probe_config_extracts_previous_secret() {
        let resp = meshmon_protocol::ConfigResponse {
            udp_probe_secret: vec![1, 2, 3, 4, 5, 6, 7, 8].into(),
            udp_probe_previous_secret: vec![9, 10, 11, 12, 13, 14, 15, 16].into(),
            ..Default::default()
        };
        let cfg = ProbeConfig::from_proto(resp).expect("valid");
        assert_eq!(cfg.udp_probe_secret, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            cfg.udp_probe_previous_secret,
            Some([9, 10, 11, 12, 13, 14, 15, 16])
        );
    }

    #[test]
    fn probe_config_rejects_wrong_length_secret() {
        let resp = meshmon_protocol::ConfigResponse {
            udp_probe_secret: vec![1, 2, 3, 4].into(),
            ..Default::default()
        };
        let err = ProbeConfig::from_proto(resp).unwrap_err();
        assert!(err.to_string().contains("udp_probe_secret"), "{err}");
    }

    #[test]
    fn probe_config_rejects_wrong_length_previous_secret() {
        let resp = meshmon_protocol::ConfigResponse {
            udp_probe_secret: vec![1, 2, 3, 4, 5, 6, 7, 8].into(),
            udp_probe_previous_secret: vec![1, 2, 3].into(),
            ..Default::default()
        };
        let err = ProbeConfig::from_proto(resp).unwrap_err();
        assert!(
            err.to_string().contains("udp_probe_previous_secret"),
            "{err}"
        );
    }

    #[test]
    fn probe_config_extracts_windows() {
        let resp = meshmon_protocol::ConfigResponse {
            udp_probe_secret: vec![1, 2, 3, 4, 5, 6, 7, 8].into(),
            windows: Some(meshmon_protocol::Windows {
                primary_sec: 120,
                diversity_sec: 600,
            }),
            ..Default::default()
        };
        let cfg = ProbeConfig::from_proto(resp).expect("valid");
        assert_eq!(cfg.primary_window_sec, 120);
        assert_eq!(cfg.diversity_window_sec, 600);
    }

    #[test]
    fn probe_config_falls_back_to_default_windows() {
        let resp = meshmon_protocol::ConfigResponse {
            udp_probe_secret: vec![1, 2, 3, 4, 5, 6, 7, 8].into(),
            windows: None,
            ..Default::default()
        };
        let cfg = ProbeConfig::from_proto(resp).expect("valid");
        assert_eq!(cfg.primary_window_sec, 300);
        assert_eq!(cfg.diversity_window_sec, 900);
    }

    #[test]
    fn priority_list_defaults_when_empty() {
        let resp = meshmon_protocol::ConfigResponse {
            udp_probe_secret: vec![1u8; 8].into(),
            ..Default::default()
        };
        let cfg = ProbeConfig::from_proto(resp).unwrap();
        assert_eq!(
            cfg.priority_list(),
            vec![Protocol::Icmp, Protocol::Tcp, Protocol::Udp]
        );
    }

    #[test]
    fn priority_list_honors_service_order() {
        let resp = meshmon_protocol::ConfigResponse {
            udp_probe_secret: vec![1u8; 8].into(),
            priority: vec![Protocol::Udp as i32, Protocol::Icmp as i32],
            ..Default::default()
        };
        let cfg = ProbeConfig::from_proto(resp).unwrap();
        assert_eq!(cfg.priority_list(), vec![Protocol::Udp, Protocol::Icmp]);
    }

    #[test]
    fn rates_for_returns_none_when_missing() {
        let resp = meshmon_protocol::ConfigResponse {
            udp_probe_secret: vec![1u8; 8].into(),
            ..Default::default()
        };
        let cfg = ProbeConfig::from_proto(resp).unwrap();
        assert!(cfg
            .rates_for(Protocol::Icmp, PbPathHealth::Normal)
            .is_none());
    }

    #[test]
    fn rates_for_finds_matching_row() {
        let resp = meshmon_protocol::ConfigResponse {
            udp_probe_secret: vec![1u8; 8].into(),
            rates: vec![meshmon_protocol::RateEntry {
                primary: Protocol::Icmp as i32,
                health: PbPathHealth::Normal as i32,
                icmp_pps: 0.2,
                tcp_pps: 0.05,
                udp_pps: 0.05,
            }],
            ..Default::default()
        };
        let cfg = ProbeConfig::from_proto(resp).unwrap();
        let row = cfg.rates_for(Protocol::Icmp, PbPathHealth::Normal).unwrap();
        assert_eq!(row.icmp_pps, 0.2);
        assert_eq!(row.tcp_pps, 0.05);
        assert_eq!(row.udp_pps, 0.05);
    }

    #[test]
    fn thresholds_for_returns_tcp_defaults() {
        let resp = meshmon_protocol::ConfigResponse {
            udp_probe_secret: vec![1u8; 8].into(),
            ..Default::default()
        };
        let cfg = ProbeConfig::from_proto(resp).unwrap();
        let t = cfg.thresholds_for(Protocol::Tcp);
        assert!((t.unhealthy_trigger_pct - 0.5).abs() < 1e-9);
        assert!((t.healthy_recovery_pct - 0.05).abs() < 1e-9);
    }

    #[test]
    fn diff_detection_returns_service_value_when_present() {
        let resp = meshmon_protocol::ConfigResponse {
            udp_probe_secret: vec![0u8; 8].into(),
            diff_detection: Some(DiffDetection {
                new_ip_min_freq: 0.30,
                hop_count_change: 2,
            }),
            ..Default::default()
        };
        let cfg = ProbeConfig::from_proto(resp).expect("valid");
        let d = cfg.diff_detection();
        assert!((d.new_ip_min_freq - 0.30).abs() < 1e-9);
        assert_eq!(d.hop_count_change, 2);
    }

    #[test]
    fn diff_detection_falls_back_to_spec_defaults() {
        let resp = meshmon_protocol::ConfigResponse {
            udp_probe_secret: vec![0u8; 8].into(),
            ..Default::default()
        };
        let cfg = ProbeConfig::from_proto(resp).expect("valid");
        let d = cfg.diff_detection();
        assert!((d.new_ip_min_freq - 0.20).abs() < 1e-9);
        assert_eq!(d.hop_count_change, 1);
    }
}

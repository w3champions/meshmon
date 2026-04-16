//! Environment-based configuration for the meshmon agent.
//!
//! [`AgentEnv`] reads required environment variables at startup and validates
//! them eagerly, reporting *all* missing or invalid values at once so the
//! operator can fix everything in a single pass.

use std::net::IpAddr;

use anyhow::{bail, Result};

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
    pub raw: meshmon_protocol::ConfigResponse,
}

impl ProbeConfig {
    /// Wrap a `ConfigResponse` received from the service.
    pub fn from_proto(resp: meshmon_protocol::ConfigResponse) -> Self {
        Self { raw: resp }
    }
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

    /// All env var names touched by the agent config.
    const ALL_VARS: [&str; 8] = [
        "MESHMON_SERVICE_URL",
        "MESHMON_AGENT_TOKEN",
        "AGENT_ID",
        "AGENT_DISPLAY_NAME",
        "AGENT_LOCATION",
        "AGENT_IP",
        "AGENT_LAT",
        "AGENT_LON",
    ];

    /// Helper: hold the env lock, clear all agent vars, set the valid
    /// defaults, run the closure, then clean up. Ensures no env leakage
    /// between parallel tests.
    fn with_valid_env(f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Start from a clean slate.
        for k in &ALL_VARS {
            env::remove_var(k);
        }
        let vars = [
            ("MESHMON_SERVICE_URL", "https://meshmon.example.com:443"),
            ("MESHMON_AGENT_TOKEN", "test-token-123"),
            ("AGENT_ID", "brazil-north"),
            ("AGENT_DISPLAY_NAME", "Brazil North"),
            ("AGENT_LOCATION", "Fortaleza, Brazil"),
            ("AGENT_IP", "170.80.110.90"),
            ("AGENT_LAT", "-3.7172"),
            ("AGENT_LON", "-38.5433"),
        ];
        for (k, v) in &vars {
            env::set_var(k, v);
        }
        f();
        for (k, _) in &vars {
            env::remove_var(k);
        }
    }

    /// Helper: hold the env lock, clear all agent vars, run the closure,
    /// then clean up.
    fn with_cleared_env(f: impl FnOnce()) {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for k in &ALL_VARS {
            env::remove_var(k);
        }
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
            for var in &ALL_VARS {
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
}

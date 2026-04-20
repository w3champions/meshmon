//! Probing configuration broadcast to agents via the `GetConfig` RPC.
//!
//! Defaults match spec 02 ("Rates per mode", state-machine thresholds,
//! window sizes, diff-detection thresholds, path-health thresholds).
//! Operators override any knob via `[probing]` in `meshmon.toml`; SIGHUP
//! reloads them through the existing `ArcSwap<Config>`.

use meshmon_protocol::{PathHealth, Protocol};
use serde::Deserialize;

/// Full probing configuration, broadcast to every connected agent via
/// `GetConfig`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbingSection {
    /// Protocols the agent should probe.
    pub enabled_protocols: Vec<Protocol>,
    /// Preference order for selecting the primary protocol.
    pub priority: Vec<Protocol>,
    /// Per-(primary, path-health) probe rate table.
    pub rates: Vec<ProbingRate>,
    /// State-machine thresholds for ICMP.
    pub icmp_thresholds: ProtocolThresholds,
    /// State-machine thresholds for TCP.
    pub tcp_thresholds: ProtocolThresholds,
    /// State-machine thresholds for UDP.
    pub udp_thresholds: ProtocolThresholds,
    /// Rolling-window sizes for primary and diversity probing.
    pub windows: ProbingWindows,
    /// Diff-detection thresholds for route-change events.
    pub diff_detection: ProbingDiffDetection,
    /// Path-level health state-machine thresholds.
    pub path_health_thresholds: PathHealthThresholds,
    /// Current UDP probe secret (exactly 8 bytes). Broadcast to agents in
    /// `ConfigResponse.udp_probe_secret`.
    pub udp_probe_secret: [u8; 8],
    /// Previous UDP probe secret during rotation. `None` when no rotation
    /// is in progress. Echo listeners accept either; probers always send
    /// with `udp_probe_secret`.
    pub udp_probe_previous_secret: Option<[u8; 8]>,
}

/// One row of the probe-rate table: for a given primary protocol and path
/// health state, how many probes per second to send for each protocol.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbingRate {
    /// Primary protocol for this row.
    pub primary: Protocol,
    /// Path-health state for this row.
    pub health: PathHealth,
    /// ICMP probes per second.
    pub icmp_pps: f64,
    /// TCP probes per second.
    pub tcp_pps: f64,
    /// UDP probes per second.
    pub udp_pps: f64,
}

/// Per-protocol state-machine thresholds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProtocolThresholds {
    /// Loss fraction that triggers the unhealthy state.
    pub unhealthy_trigger_pct: f64,
    /// Loss fraction below which healthy recovery is allowed.
    pub healthy_recovery_pct: f64,
    /// Seconds the unhealthy condition must persist before transitioning.
    pub unhealthy_hysteresis_sec: u32,
    /// Seconds the healthy condition must persist before recovering.
    pub healthy_hysteresis_sec: u32,
}

/// Rolling-window sizes for probing evaluation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbingWindows {
    /// Primary-protocol evaluation window in seconds.
    pub primary_sec: u32,
    /// Diversity-protocol evaluation window in seconds.
    pub diversity_sec: u32,
}

/// Thresholds for detecting structural route-change events.
///
/// Only topology changes (new IP at a position, hop count change) are
/// diff-worthy. Per-hop loss and per-hop RTT are measurement signals and
/// live in the rolling-stats / alerting pipeline, not in route snapshots.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbingDiffDetection {
    /// Minimum observed frequency for a new IP to be considered significant.
    pub new_ip_min_freq: f64,
    /// Hop-count change that triggers a diff event.
    pub hop_count_change: u32,
}

/// Path-level health state-machine thresholds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathHealthThresholds {
    /// Loss fraction that triggers the degraded state.
    pub degraded_trigger_pct: f64,
    /// Seconds the degraded condition must persist before transitioning.
    pub degraded_trigger_sec: u32,
    /// Minimum number of samples required to evaluate degraded state.
    pub degraded_min_samples: u32,
    /// Loss fraction below which normal recovery is allowed.
    pub normal_recovery_pct: f64,
    /// Seconds the normal condition must persist before recovering.
    pub normal_recovery_sec: u32,
}

impl Default for ProbingSection {
    fn default() -> Self {
        use PathHealth::*;
        use Protocol::*;
        let rates = vec![
            ProbingRate {
                primary: Icmp,
                health: Normal,
                icmp_pps: 0.20,
                tcp_pps: 0.05,
                udp_pps: 0.05,
            },
            ProbingRate {
                primary: Icmp,
                health: Degraded,
                icmp_pps: 1.00,
                tcp_pps: 0.05,
                udp_pps: 0.05,
            },
            ProbingRate {
                primary: Icmp,
                health: Unreachable,
                icmp_pps: 1.00,
                tcp_pps: 0.05,
                udp_pps: 0.05,
            },
            ProbingRate {
                primary: Tcp,
                health: Normal,
                icmp_pps: 0.05,
                tcp_pps: 0.20,
                udp_pps: 0.05,
            },
            ProbingRate {
                primary: Tcp,
                health: Degraded,
                icmp_pps: 0.05,
                tcp_pps: 1.00,
                udp_pps: 0.05,
            },
            ProbingRate {
                primary: Tcp,
                health: Unreachable,
                icmp_pps: 0.05,
                tcp_pps: 1.00,
                udp_pps: 0.05,
            },
            ProbingRate {
                primary: Udp,
                health: Normal,
                icmp_pps: 0.05,
                tcp_pps: 0.05,
                udp_pps: 0.20,
            },
            ProbingRate {
                primary: Udp,
                health: Degraded,
                icmp_pps: 0.05,
                tcp_pps: 0.05,
                udp_pps: 1.00,
            },
            ProbingRate {
                primary: Udp,
                health: Unreachable,
                icmp_pps: 0.05,
                tcp_pps: 0.05,
                udp_pps: 1.00,
            },
        ];
        Self {
            enabled_protocols: vec![Icmp, Tcp, Udp],
            priority: vec![Icmp, Tcp, Udp],
            rates,
            icmp_thresholds: ProtocolThresholds {
                unhealthy_trigger_pct: 0.90,
                healthy_recovery_pct: 0.10,
                unhealthy_hysteresis_sec: 30,
                healthy_hysteresis_sec: 60,
            },
            tcp_thresholds: ProtocolThresholds {
                unhealthy_trigger_pct: 0.50,
                healthy_recovery_pct: 0.05,
                unhealthy_hysteresis_sec: 30,
                healthy_hysteresis_sec: 60,
            },
            udp_thresholds: ProtocolThresholds {
                unhealthy_trigger_pct: 0.90,
                healthy_recovery_pct: 0.10,
                unhealthy_hysteresis_sec: 30,
                healthy_hysteresis_sec: 60,
            },
            windows: ProbingWindows {
                primary_sec: 300,
                diversity_sec: 900,
            },
            diff_detection: ProbingDiffDetection {
                new_ip_min_freq: 0.20,
                hop_count_change: 1,
            },
            path_health_thresholds: PathHealthThresholds {
                degraded_trigger_pct: 0.05,
                degraded_trigger_sec: 120,
                degraded_min_samples: 30,
                normal_recovery_pct: 0.02,
                normal_recovery_sec: 300,
            },
            udp_probe_secret: *b"mshmn-v1",
            udp_probe_previous_secret: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Raw (TOML-deserializable) types
// ---------------------------------------------------------------------------

// Lowercase string enum for `Protocol` that TOML can deserialize.
// Converts to the prost-generated `Protocol` via `From`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RawProtocol {
    Icmp,
    Tcp,
    Udp,
}

impl From<RawProtocol> for Protocol {
    fn from(r: RawProtocol) -> Self {
        match r {
            RawProtocol::Icmp => Protocol::Icmp,
            RawProtocol::Tcp => Protocol::Tcp,
            RawProtocol::Udp => Protocol::Udp,
        }
    }
}

// Lowercase string enum for `PathHealth` that TOML can deserialize.
// Converts to the prost-generated `PathHealth` via `From`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RawPathHealth {
    Normal,
    Degraded,
    Unreachable,
}

impl From<RawPathHealth> for PathHealth {
    fn from(r: RawPathHealth) -> Self {
        match r {
            RawPathHealth::Normal => PathHealth::Normal,
            RawPathHealth::Degraded => PathHealth::Degraded,
            RawPathHealth::Unreachable => PathHealth::Unreachable,
        }
    }
}

// On-disk override for a single rate-table row. All pps fields are required
// when a rate entry is provided (partial override semantics for rates is
// ambiguous — if the operator specifies any rates, they must supply the full set).
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RawProbingRate {
    primary: RawProtocol,
    health: RawPathHealth,
    icmp_pps: f64,
    tcp_pps: f64,
    udp_pps: f64,
}

// On-disk override shape for `ProtocolThresholds`. Every field is optional;
// absent fields keep the spec-02 default.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawProtocolThresholds {
    unhealthy_trigger_pct: Option<f64>,
    healthy_recovery_pct: Option<f64>,
    unhealthy_hysteresis_sec: Option<u32>,
    healthy_hysteresis_sec: Option<u32>,
}

// On-disk override shape for `ProbingWindows`. Every field is optional.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawProbingWindows {
    primary_sec: Option<u32>,
    diversity_sec: Option<u32>,
}

// On-disk override shape for `ProbingDiffDetection`. Every field is optional.
// Unknown keys are silently ignored so legacy operator configs that still
// carry the removed `missing_ip_max_freq` / `rtt_shift_frac` keys don't
// refuse to boot.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawProbingDiffDetection {
    new_ip_min_freq: Option<f64>,
    hop_count_change: Option<u32>,
}

// On-disk override shape for `PathHealthThresholds`. Every field is optional.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RawPathHealthThresholds {
    degraded_trigger_pct: Option<f64>,
    degraded_trigger_sec: Option<u32>,
    degraded_min_samples: Option<u32>,
    normal_recovery_pct: Option<f64>,
    normal_recovery_sec: Option<u32>,
}

/// The `[probing]` section as it appears on disk. Every field is optional;
/// absent fields yield the spec-02 default. When any `[[rates]]` entry is
/// provided, the full `priority × {Normal, Degraded, Unreachable}` matrix
/// must be covered.
#[derive(Debug, Default, Deserialize)]
pub struct RawProbingSection {
    enabled_protocols: Option<Vec<RawProtocol>>,
    priority: Option<Vec<RawProtocol>>,
    #[serde(default)]
    rates: Vec<RawProbingRate>,
    #[serde(default)]
    icmp_thresholds: RawProtocolThresholds,
    #[serde(default)]
    tcp_thresholds: RawProtocolThresholds,
    #[serde(default)]
    udp_thresholds: RawProtocolThresholds,
    #[serde(default)]
    windows: RawProbingWindows,
    #[serde(default)]
    diff_detection: RawProbingDiffDetection,
    #[serde(default)]
    path_health_thresholds: RawPathHealthThresholds,
    udp_probe_secret: Option<String>,
    udp_probe_secret_env: Option<String>,
    udp_probe_previous_secret: Option<String>,
    udp_probe_previous_secret_env: Option<String>,
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

fn validate_fraction(name: &str, v: f64) -> Result<(), String> {
    if (0.0..=1.0).contains(&v) {
        Ok(())
    } else {
        Err(format!("`{name}` = {v} is out of range [0.0, 1.0]"))
    }
}

fn validate_positive_u32(name: &str, v: u32) -> Result<(), String> {
    if v > 0 {
        Ok(())
    } else {
        Err(format!("`{name}` must be > 0 (zero is not allowed)"))
    }
}

fn validate_min_u32(name: &str, v: u32, min: u32) -> Result<(), String> {
    if v >= min {
        Ok(())
    } else {
        Err(format!("`{name}` = {v} must be >= {min}"))
    }
}

/// Validate a probe rate in packets-per-second. Zero means "never probe
/// this (primary × health × protocol) combination" — semantically valid
/// when operators want to disable a probe type without removing its row
/// from the rate matrix. NaN/Inf/negative values have no meaningful
/// scheduler interpretation and are rejected at config load.
fn validate_pps(name: &str, v: f64) -> Result<(), String> {
    if v.is_finite() && v >= 0.0 {
        Ok(())
    } else {
        Err(format!("`{name}` = {v} must be finite and >= 0"))
    }
}

// ---------------------------------------------------------------------------
// TryFrom conversion
// ---------------------------------------------------------------------------

impl TryFrom<RawProbingSection> for ProbingSection {
    type Error = String;

    fn try_from(raw: RawProbingSection) -> Result<Self, Self::Error> {
        let defaults = ProbingSection::default();

        // --- enabled_protocols ---
        let enabled_protocols: Vec<Protocol> = raw
            .enabled_protocols
            .map(|v| v.into_iter().map(Protocol::from).collect())
            .unwrap_or_else(|| defaults.enabled_protocols.clone());

        // --- priority ---
        let priority: Vec<Protocol> = raw
            .priority
            .map(|v| v.into_iter().map(Protocol::from).collect())
            .unwrap_or_else(|| defaults.priority.clone());

        // Invariant: priority must be a subset of enabled_protocols.
        for p in &priority {
            if !enabled_protocols.contains(p) {
                return Err(format!(
                    "priority protocol `{p:?}` is not in `enabled_protocols` — \
                     priority must be a subset of enabled protocols"
                ));
            }
        }

        // --- rates ---
        let rates: Vec<ProbingRate> = if raw.rates.is_empty() {
            defaults.rates.clone()
        } else {
            // When the operator provides any rates, they must cover the full
            // priority × {Normal, Degraded, Unreachable} matrix.
            let health_states = [
                PathHealth::Normal,
                PathHealth::Degraded,
                PathHealth::Unreachable,
            ];
            for primary in &priority {
                for health in health_states {
                    let covered = raw.rates.iter().any(|r| {
                        Protocol::from(r.primary) == *primary
                            && PathHealth::from(r.health) == health
                    });
                    if !covered {
                        return Err(format!(
                            "rate entry for (primary={primary:?}, health={health:?}) is missing — \
                             when any `[[rates]]` entries are provided, all \
                             priority × {{Normal, Degraded, Unreachable}} combinations must be covered"
                        ));
                    }
                }
            }
            raw.rates
                .into_iter()
                .map(|r| {
                    let primary = Protocol::from(r.primary);
                    let health = PathHealth::from(r.health);
                    validate_pps(
                        &format!("rates[primary={primary:?},health={health:?}].icmp_pps"),
                        r.icmp_pps,
                    )?;
                    validate_pps(
                        &format!("rates[primary={primary:?},health={health:?}].tcp_pps"),
                        r.tcp_pps,
                    )?;
                    validate_pps(
                        &format!("rates[primary={primary:?},health={health:?}].udp_pps"),
                        r.udp_pps,
                    )?;
                    Ok::<_, String>(ProbingRate {
                        primary,
                        health,
                        icmp_pps: r.icmp_pps,
                        tcp_pps: r.tcp_pps,
                        udp_pps: r.udp_pps,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        };

        // --- icmp_thresholds ---
        let icmp_thresholds = {
            let d = defaults.icmp_thresholds;
            let ut = raw
                .icmp_thresholds
                .unhealthy_trigger_pct
                .unwrap_or(d.unhealthy_trigger_pct);
            let hr = raw
                .icmp_thresholds
                .healthy_recovery_pct
                .unwrap_or(d.healthy_recovery_pct);
            let uh = raw
                .icmp_thresholds
                .unhealthy_hysteresis_sec
                .unwrap_or(d.unhealthy_hysteresis_sec);
            let hh = raw
                .icmp_thresholds
                .healthy_hysteresis_sec
                .unwrap_or(d.healthy_hysteresis_sec);
            validate_fraction("icmp_thresholds.unhealthy_trigger_pct", ut)?;
            validate_fraction("icmp_thresholds.healthy_recovery_pct", hr)?;
            validate_positive_u32("icmp_thresholds.unhealthy_hysteresis_sec", uh)?;
            validate_positive_u32("icmp_thresholds.healthy_hysteresis_sec", hh)?;
            ProtocolThresholds {
                unhealthy_trigger_pct: ut,
                healthy_recovery_pct: hr,
                unhealthy_hysteresis_sec: uh,
                healthy_hysteresis_sec: hh,
            }
        };

        // --- tcp_thresholds ---
        let tcp_thresholds = {
            let d = defaults.tcp_thresholds;
            let ut = raw
                .tcp_thresholds
                .unhealthy_trigger_pct
                .unwrap_or(d.unhealthy_trigger_pct);
            let hr = raw
                .tcp_thresholds
                .healthy_recovery_pct
                .unwrap_or(d.healthy_recovery_pct);
            let uh = raw
                .tcp_thresholds
                .unhealthy_hysteresis_sec
                .unwrap_or(d.unhealthy_hysteresis_sec);
            let hh = raw
                .tcp_thresholds
                .healthy_hysteresis_sec
                .unwrap_or(d.healthy_hysteresis_sec);
            validate_fraction("tcp_thresholds.unhealthy_trigger_pct", ut)?;
            validate_fraction("tcp_thresholds.healthy_recovery_pct", hr)?;
            validate_positive_u32("tcp_thresholds.unhealthy_hysteresis_sec", uh)?;
            validate_positive_u32("tcp_thresholds.healthy_hysteresis_sec", hh)?;
            ProtocolThresholds {
                unhealthy_trigger_pct: ut,
                healthy_recovery_pct: hr,
                unhealthy_hysteresis_sec: uh,
                healthy_hysteresis_sec: hh,
            }
        };

        // --- udp_thresholds ---
        let udp_thresholds = {
            let d = defaults.udp_thresholds;
            let ut = raw
                .udp_thresholds
                .unhealthy_trigger_pct
                .unwrap_or(d.unhealthy_trigger_pct);
            let hr = raw
                .udp_thresholds
                .healthy_recovery_pct
                .unwrap_or(d.healthy_recovery_pct);
            let uh = raw
                .udp_thresholds
                .unhealthy_hysteresis_sec
                .unwrap_or(d.unhealthy_hysteresis_sec);
            let hh = raw
                .udp_thresholds
                .healthy_hysteresis_sec
                .unwrap_or(d.healthy_hysteresis_sec);
            validate_fraction("udp_thresholds.unhealthy_trigger_pct", ut)?;
            validate_fraction("udp_thresholds.healthy_recovery_pct", hr)?;
            validate_positive_u32("udp_thresholds.unhealthy_hysteresis_sec", uh)?;
            validate_positive_u32("udp_thresholds.healthy_hysteresis_sec", hh)?;
            ProtocolThresholds {
                unhealthy_trigger_pct: ut,
                healthy_recovery_pct: hr,
                unhealthy_hysteresis_sec: uh,
                healthy_hysteresis_sec: hh,
            }
        };

        // --- windows ---
        let windows = {
            let d = defaults.windows;
            let ps = raw.windows.primary_sec.unwrap_or(d.primary_sec);
            let ds = raw.windows.diversity_sec.unwrap_or(d.diversity_sec);
            validate_positive_u32("windows.primary_sec", ps)?;
            validate_positive_u32("windows.diversity_sec", ds)?;
            ProbingWindows {
                primary_sec: ps,
                diversity_sec: ds,
            }
        };

        // --- diff_detection ---
        let diff_detection = {
            let d = defaults.diff_detection;
            let nim = raw
                .diff_detection
                .new_ip_min_freq
                .unwrap_or(d.new_ip_min_freq);
            let hcc = raw
                .diff_detection
                .hop_count_change
                .unwrap_or(d.hop_count_change);
            validate_fraction("diff_detection.new_ip_min_freq", nim)?;
            validate_positive_u32("diff_detection.hop_count_change", hcc)?;
            ProbingDiffDetection {
                new_ip_min_freq: nim,
                hop_count_change: hcc,
            }
        };

        // --- path_health_thresholds ---
        let path_health_thresholds = {
            let d = defaults.path_health_thresholds;
            let dtp = raw
                .path_health_thresholds
                .degraded_trigger_pct
                .unwrap_or(d.degraded_trigger_pct);
            let dts = raw
                .path_health_thresholds
                .degraded_trigger_sec
                .unwrap_or(d.degraded_trigger_sec);
            let dms = raw
                .path_health_thresholds
                .degraded_min_samples
                .unwrap_or(d.degraded_min_samples);
            let nrp = raw
                .path_health_thresholds
                .normal_recovery_pct
                .unwrap_or(d.normal_recovery_pct);
            let nrs = raw
                .path_health_thresholds
                .normal_recovery_sec
                .unwrap_or(d.normal_recovery_sec);
            validate_fraction("path_health_thresholds.degraded_trigger_pct", dtp)?;
            validate_fraction("path_health_thresholds.normal_recovery_pct", nrp)?;
            validate_positive_u32("path_health_thresholds.degraded_trigger_sec", dts)?;
            validate_positive_u32("path_health_thresholds.normal_recovery_sec", nrs)?;
            // Match the agent state machine's hard floor (MIN_TRANSITION_SAMPLES
            // = 3 in crates/agent/src/state.rs). Values below 3 are clamped
            // there regardless, so fail fast here instead of shipping a config
            // that silently behaves differently than written.
            validate_min_u32("path_health_thresholds.degraded_min_samples", dms, 3)?;
            PathHealthThresholds {
                degraded_trigger_pct: dtp,
                degraded_trigger_sec: dts,
                degraded_min_samples: dms,
                normal_recovery_pct: nrp,
                normal_recovery_sec: nrs,
            }
        };

        // --- udp_probe_secret (required) ---
        let udp_probe_secret_str = resolve_probing_secret_required(
            "udp_probe_secret",
            raw.udp_probe_secret.as_deref(),
            raw.udp_probe_secret_env.as_deref(),
        )?;
        let udp_probe_secret =
            parse_secret_required("udp_probe_secret", Some(&udp_probe_secret_str))?;
        let udp_probe_previous_secret = match resolve_probing_secret_optional(
            "udp_probe_previous_secret",
            raw.udp_probe_previous_secret.as_deref(),
            raw.udp_probe_previous_secret_env.as_deref(),
        )? {
            Some(s) => Some(parse_secret_required(
                "udp_probe_previous_secret",
                Some(&s),
            )?),
            None => None,
        };

        Ok(ProbingSection {
            enabled_protocols,
            priority,
            rates,
            icmp_thresholds,
            tcp_thresholds,
            udp_thresholds,
            windows,
            diff_detection,
            path_health_thresholds,
            udp_probe_secret,
            udp_probe_previous_secret,
        })
    }
}

/// Resolve a probing secret from either inline TOML (`<key>`) or env-var
/// indirection (`<key>_env`). Mutually exclusive; both-set is rejected.
///
/// Empty values — whether inline or resolved from env — are treated as
/// "not set" and rejected, matching the posture used for other secrets
/// in `config.rs::resolve_secret`. An env var declared via `_env` but
/// absent from the process environment is a configuration error
/// (opt-in to env indirection + typo).
fn resolve_probing_secret(
    key: &str,
    inline: Option<&str>,
    env_name: Option<&str>,
) -> Result<Option<String>, String> {
    match (inline, env_name) {
        (Some(_), Some(_)) => Err(format!("`{key}` and `{key}_env` are mutually exclusive")),
        (Some(v), None) => {
            if v.is_empty() {
                Err(format!("`{key}` is set to an empty string"))
            } else {
                Ok(Some(v.to_string()))
            }
        }
        (None, Some(name)) => match std::env::var(name) {
            Ok(v) if !v.is_empty() => Ok(Some(v)),
            Ok(_) => Err(format!(
                "env var `{name}` (for `{key}_env`) is set but empty"
            )),
            Err(_) => Err(format!("env var `{name}` (for `{key}_env`) is not set")),
        },
        (None, None) => Ok(None),
    }
}

fn resolve_probing_secret_required(
    key: &str,
    inline: Option<&str>,
    env_name: Option<&str>,
) -> Result<String, String> {
    resolve_probing_secret(key, inline, env_name)?
        .ok_or_else(|| format!("`{key}` or `{key}_env` is required"))
}

fn resolve_probing_secret_optional(
    key: &str,
    inline: Option<&str>,
    env_name: Option<&str>,
) -> Result<Option<String>, String> {
    resolve_probing_secret(key, inline, env_name)
}

/// Parse an 8-byte secret from a `hex:...` or `base64:...` prefixed string.
fn parse_secret_required(field: &str, raw: Option<&str>) -> Result<[u8; 8], String> {
    let s = raw.ok_or_else(|| format!("`{field}` is required"))?;
    let bytes = decode_prefixed_bytes(field, s)?;
    if bytes.len() != 8 {
        return Err(format!(
            "`{field}` must decode to exactly 8 bytes, got {}",
            bytes.len()
        ));
    }
    let mut out = [0u8; 8];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn decode_prefixed_bytes(field: &str, raw: &str) -> Result<Vec<u8>, String> {
    if let Some(hex) = raw.strip_prefix("hex:") {
        hex_decode(hex).map_err(|e| format!("`{field}` hex decode: {e}"))
    } else if let Some(b64) = raw.strip_prefix("base64:") {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| format!("`{field}` base64 decode: {e}"))
    } else {
        Err(format!("`{field}` must start with `hex:` or `base64:`"))
    }
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    // Guard against non-ASCII input (e.g. multi-byte UTF-8). Without this
    // check the byte-slice below (`&s[i..i + 2]`) panics on a boundary
    // that splits a multi-byte character, which turns a malformed operator
    // config value into a crash instead of a validation error.
    if !s.is_ascii() {
        return Err("hex input must be ASCII".to_string());
    }
    if !s.len().is_multiple_of(2) {
        return Err(format!("odd length {}", s.len()));
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..s.len()).step_by(2) {
        let byte =
            u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("at offset {i}: {e}"))?;
        out.push(byte);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_cover_every_priority_pair() {
        let cfg = ProbingSection::default();
        for primary in &cfg.priority {
            for health in [
                PathHealth::Normal,
                PathHealth::Degraded,
                PathHealth::Unreachable,
            ] {
                assert!(
                    cfg.rates
                        .iter()
                        .any(|r| r.primary == *primary && r.health == health),
                    "missing rate entry for {primary:?} / {health:?}"
                );
            }
        }
    }

    #[test]
    fn defaults_fractions_in_range() {
        let cfg = ProbingSection::default();
        for (name, v) in [
            ("icmp.unhealthy", cfg.icmp_thresholds.unhealthy_trigger_pct),
            ("icmp.recovery", cfg.icmp_thresholds.healthy_recovery_pct),
            ("tcp.unhealthy", cfg.tcp_thresholds.unhealthy_trigger_pct),
            ("tcp.recovery", cfg.tcp_thresholds.healthy_recovery_pct),
            ("udp.unhealthy", cfg.udp_thresholds.unhealthy_trigger_pct),
            ("udp.recovery", cfg.udp_thresholds.healthy_recovery_pct),
            ("diff.new_ip", cfg.diff_detection.new_ip_min_freq),
            (
                "path.degraded",
                cfg.path_health_thresholds.degraded_trigger_pct,
            ),
            (
                "path.normal",
                cfg.path_health_thresholds.normal_recovery_pct,
            ),
        ] {
            assert!((0.0..=1.0).contains(&v), "{name} = {v} out of range");
        }
    }

    // -----------------------------------------------------------------------
    // New Task-14 tests
    // -----------------------------------------------------------------------

    #[test]
    fn empty_toml_yields_defaults() {
        let raw: RawProbingSection =
            toml::from_str(r#"udp_probe_secret = "hex:6d73686d6e2d7631""#).unwrap();
        let cfg = ProbingSection::try_from(raw).unwrap();
        assert_eq!(cfg, ProbingSection::default());
    }

    #[test]
    fn override_primary_window() {
        let raw: RawProbingSection = toml::from_str(
            r#"
            udp_probe_secret = "hex:6d73686d6e2d7631"

            [windows]
            primary_sec = 120
        "#,
        )
        .unwrap();
        let cfg = ProbingSection::try_from(raw).unwrap();
        assert_eq!(cfg.windows.primary_sec, 120);
        assert_eq!(cfg.windows.diversity_sec, 900); // default preserved
    }

    #[test]
    fn priority_subset_of_enabled() {
        // Priority contains UDP but enabled_protocols does not => error.
        let raw: RawProbingSection = toml::from_str(
            r#"
            enabled_protocols = ["icmp", "tcp"]
            priority = ["icmp", "udp"]
        "#,
        )
        .unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        assert!(
            err.to_lowercase().contains("priority"),
            "expected 'priority' in error, got: {err}"
        );
    }

    #[test]
    fn rates_cover_priority_x_health() {
        // Provide only one rate entry for Udp×Normal when the default priority
        // includes Udp — Udp×Degraded and Udp×Unreachable are missing.
        let raw: RawProbingSection = toml::from_str(
            r#"
            [[rates]]
            primary = "udp"
            health = "normal"
            icmp_pps = 0.1
            tcp_pps = 0.1
            udp_pps = 0.1
        "#,
        )
        .unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        assert!(
            err.to_lowercase().contains("rate"),
            "expected 'rate' in error, got: {err}"
        );
    }

    #[test]
    fn fraction_out_of_range_rejected() {
        let raw: RawProbingSection = toml::from_str(
            r#"
            [icmp_thresholds]
            unhealthy_trigger_pct = 1.5
        "#,
        )
        .unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        assert!(
            err.to_lowercase().contains("icmp") || err.to_lowercase().contains("pct"),
            "expected 'icmp' or 'pct' in error, got: {err}"
        );
    }

    #[test]
    fn zero_window_rejected() {
        let raw: RawProbingSection = toml::from_str(
            r#"
            [windows]
            primary_sec = 0
        "#,
        )
        .unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        assert!(
            err.to_lowercase().contains("window") || err.to_lowercase().contains("primary"),
            "expected 'window' or 'primary' in error, got: {err}"
        );
    }

    #[test]
    fn zero_degraded_min_samples_rejected() {
        let raw: RawProbingSection = toml::from_str(
            r#"
            [path_health_thresholds]
            degraded_min_samples = 0
        "#,
        )
        .unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        assert!(
            err.to_lowercase().contains("degraded_min_samples"),
            "expected 'degraded_min_samples' in error, got: {err}"
        );
    }

    #[test]
    fn degraded_min_samples_below_transition_floor_rejected() {
        let raw: RawProbingSection = toml::from_str(
            r#"
            [path_health_thresholds]
            degraded_min_samples = 2
        "#,
        )
        .unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        assert!(
            err.to_lowercase().contains("degraded_min_samples") && err.contains(">= 3"),
            "expected 'degraded_min_samples' and '>= 3' in error, got: {err}"
        );
    }

    #[test]
    fn degraded_min_samples_at_floor_accepted() {
        // `udp_probe_secret` satisfies an unrelated required-field
        // validation so the test actually reaches the path_health block.
        let raw: RawProbingSection = toml::from_str(
            r#"
            udp_probe_secret = "hex:6d73686d6e2d7631"
            [path_health_thresholds]
            degraded_min_samples = 3
        "#,
        )
        .unwrap();
        ProbingSection::try_from(raw).expect("degraded_min_samples = 3 should be accepted");
    }

    #[test]
    fn zero_hop_count_change_rejected() {
        let raw: RawProbingSection = toml::from_str(
            r#"
            [diff_detection]
            hop_count_change = 0
        "#,
        )
        .unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        assert!(
            err.to_lowercase().contains("hop_count_change"),
            "expected 'hop_count_change' in error, got: {err}"
        );
    }

    #[test]
    fn negative_pps_rejected() {
        // NaN / Inf / negative probe rates have no meaningful scheduler
        // interpretation. Zero is accepted (see validate_pps doc comment).
        // Priority is narrowed to icmp only so the rate matrix only needs
        // 3 entries (primary=icmp × 3 health states) instead of 9.
        let raw: RawProbingSection = toml::from_str(
            r#"
            enabled_protocols = ["icmp"]
            priority = ["icmp"]

            [[rates]]
            primary = "icmp"
            health = "normal"
            icmp_pps = -0.1
            tcp_pps = 0.05
            udp_pps = 0.05

            [[rates]]
            primary = "icmp"
            health = "degraded"
            icmp_pps = 0.05
            tcp_pps = 0.05
            udp_pps = 0.05

            [[rates]]
            primary = "icmp"
            health = "unreachable"
            icmp_pps = 0.05
            tcp_pps = 0.05
            udp_pps = 0.05
        "#,
        )
        .unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        assert!(
            err.to_lowercase().contains("icmp_pps"),
            "expected 'icmp_pps' in error, got: {err}"
        );
    }

    #[test]
    fn nan_pps_rejected() {
        let raw: RawProbingSection = toml::from_str(
            r#"
            enabled_protocols = ["icmp"]
            priority = ["icmp"]

            [[rates]]
            primary = "icmp"
            health = "normal"
            icmp_pps = nan
            tcp_pps = 0.05
            udp_pps = 0.05

            [[rates]]
            primary = "icmp"
            health = "degraded"
            icmp_pps = 0.05
            tcp_pps = 0.05
            udp_pps = 0.05

            [[rates]]
            primary = "icmp"
            health = "unreachable"
            icmp_pps = 0.05
            tcp_pps = 0.05
            udp_pps = 0.05
        "#,
        )
        .unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        assert!(
            err.to_lowercase().contains("icmp_pps"),
            "expected 'icmp_pps' in error, got: {err}"
        );
    }

    #[test]
    fn parses_hex_secret() {
        let toml_src = r#"udp_probe_secret = "hex:0011223344556677""#;
        let raw: RawProbingSection = toml::from_str(toml_src).unwrap();
        let parsed = ProbingSection::try_from(raw).unwrap();
        assert_eq!(
            parsed.udp_probe_secret,
            [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77]
        );
        assert!(parsed.udp_probe_previous_secret.is_none());
    }

    #[test]
    fn parses_base64_secret_and_previous() {
        let toml_src = r#"
            udp_probe_secret = "base64:AAECAwQFBgc="
            udp_probe_previous_secret = "hex:7766554433221100"
        "#;
        let raw: RawProbingSection = toml::from_str(toml_src).unwrap();
        let parsed = ProbingSection::try_from(raw).unwrap();
        assert_eq!(parsed.udp_probe_secret, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(
            parsed.udp_probe_previous_secret,
            Some([0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00]),
        );
    }

    #[test]
    fn rejects_secret_wrong_length() {
        let toml_src = r#"udp_probe_secret = "hex:0011""#;
        let raw: RawProbingSection = toml::from_str(toml_src).unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        assert!(err.contains("8 bytes"), "{err}");
    }

    #[test]
    fn rejects_secret_bad_prefix() {
        let toml_src = r#"udp_probe_secret = "plain:abcd""#;
        let raw: RawProbingSection = toml::from_str(toml_src).unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        assert!(err.contains("hex:"), "{err}");
    }

    #[test]
    fn rejects_missing_secret() {
        let raw: RawProbingSection = toml::from_str("").unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        assert!(err.contains("udp_probe_secret"), "{err}");
    }

    #[test]
    fn resolves_secret_from_env_var() {
        let var = "MESHMON_TEST_UDP_PROBE_SECRET_RESOLVES";
        std::env::set_var(var, "hex:0011223344556677");
        let toml_src = format!(r#"udp_probe_secret_env = "{var}""#);
        let raw: RawProbingSection = toml::from_str(&toml_src).unwrap();
        let parsed = ProbingSection::try_from(raw).unwrap();
        std::env::remove_var(var);
        assert_eq!(
            parsed.udp_probe_secret,
            [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77]
        );
    }

    #[test]
    fn rejects_both_inline_and_env_for_secret() {
        let toml_src = r#"
            udp_probe_secret = "hex:0011223344556677"
            udp_probe_secret_env = "MESHMON_TEST_UDP_PROBE_SECRET_BOTH"
        "#;
        let raw: RawProbingSection = toml::from_str(toml_src).unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        assert!(
            err.contains("mutually exclusive"),
            "expected mutually-exclusive error, got: {err}",
        );
    }

    #[test]
    fn rejects_unset_env_var_for_secret() {
        let toml_src = r#"udp_probe_secret_env = "MESHMON_TEST_UDP_PROBE_SECRET_UNSET_XYZ""#;
        let raw: RawProbingSection = toml::from_str(toml_src).unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        assert!(err.contains("not set"), "{err}");
    }

    #[test]
    fn rejects_empty_env_var_for_secret() {
        let var = "MESHMON_TEST_UDP_PROBE_SECRET_EMPTY";
        std::env::set_var(var, "");
        let toml_src = format!(r#"udp_probe_secret_env = "{var}""#);
        let raw: RawProbingSection = toml::from_str(&toml_src).unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        std::env::remove_var(var);
        assert!(err.contains("empty"), "{err}");
    }

    #[test]
    fn resolves_previous_secret_from_env_var() {
        let var = "MESHMON_TEST_UDP_PROBE_PREV_RESOLVES";
        std::env::set_var(var, "hex:7766554433221100");
        let toml_src = format!(
            r#"
                udp_probe_secret = "hex:0011223344556677"
                udp_probe_previous_secret_env = "{var}"
            "#
        );
        let raw: RawProbingSection = toml::from_str(&toml_src).unwrap();
        let parsed = ProbingSection::try_from(raw).unwrap();
        std::env::remove_var(var);
        assert_eq!(
            parsed.udp_probe_previous_secret,
            Some([0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00]),
        );
    }

    #[test]
    fn rejects_non_ascii_hex_secret() {
        // Non-ASCII bytes cannot be safely byte-indexed by the hex decoder.
        // A malformed operator config must surface as a validation error
        // rather than a panic. The emoji is a 4-byte UTF-8 sequence that
        // under naive byte-slicing would land mid-codepoint and panic.
        let toml_src = "udp_probe_secret = \"hex:001122\u{1F600}0033\"";
        let raw: RawProbingSection = toml::from_str(toml_src).unwrap();
        let err = ProbingSection::try_from(raw).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("ASCII") || msg.contains("ascii"),
            "expected ASCII validation error, got: {msg}",
        );
    }
}

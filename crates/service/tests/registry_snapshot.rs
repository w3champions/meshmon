//! Pure-function tests for `RegistrySnapshot`. No DB.

use chrono::{Duration, Utc};
use meshmon_service::registry::{AgentInfo, RegistrySnapshot};
use sqlx::types::ipnetwork::IpNetwork;
use std::str::FromStr;
use std::time::Duration as StdDuration;

fn mk(id: &str, last_seen_offset_minutes: i64) -> AgentInfo {
    AgentInfo {
        id: id.to_string(),
        display_name: format!("Agent {id}"),
        location: None,
        ip: IpNetwork::from_str("10.0.0.1/32").unwrap(),
        latitude: None,
        longitude: None,
        agent_version: None,
        tcp_probe_port: 3555,
        udp_probe_port: 3552,
        registered_at: Utc::now(),
        last_seen_at: Utc::now() + Duration::minutes(last_seen_offset_minutes),
        campaign_max_concurrency: None,
    }
}

#[test]
fn get_returns_none_for_unknown() {
    let snap = RegistrySnapshot::empty();
    assert!(snap.get("nope").is_none());
}

#[test]
fn get_returns_agent_when_present() {
    let snap = RegistrySnapshot::from_agents(vec![mk("a", 0)]);
    assert!(snap.get("a").is_some());
    assert_eq!(snap.get("a").unwrap().id, "a");
}

#[test]
fn all_returns_every_agent_regardless_of_staleness() {
    let snap = RegistrySnapshot::from_agents(vec![mk("fresh", 0), mk("stale", -120)]);
    let mut ids: Vec<String> = snap.all().into_iter().map(|a| a.id).collect();
    ids.sort();
    assert_eq!(ids, vec!["fresh", "stale"]);
}

#[test]
fn last_seen_seconds_returns_unix_epoch() {
    let snap = RegistrySnapshot::from_agents(vec![mk("a", 0)]);
    let ts = snap.last_seen_seconds("a").expect("present");
    assert!((ts - Utc::now().timestamp()).abs() <= 2);
    assert!(snap.last_seen_seconds("missing").is_none());
}

#[test]
fn active_targets_filters_by_window_and_excludes_self() {
    let snap =
        RegistrySnapshot::from_agents(vec![mk("self", 0), mk("fresh", -1), mk("stale", -30)]);
    let window = StdDuration::from_secs(5 * 60);
    let targets = snap.active_targets("self", window);
    let ids: Vec<&str> = targets.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, vec!["fresh"]);
}

#[test]
fn active_targets_future_last_seen_counts_as_active() {
    // Clock skew on an agent host can push last_seen_at slightly into the
    // future; we don't want to drop those.
    let snap = RegistrySnapshot::from_agents(vec![
        mk("other", 2), // 2 min in the future
    ]);
    let window = StdDuration::from_secs(60);
    assert_eq!(snap.active_targets("self", window).len(), 1);
}

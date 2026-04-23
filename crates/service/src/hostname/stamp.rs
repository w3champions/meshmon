//! Shared stamp helpers for attaching reverse-DNS hostnames to response DTOs.
//!
//! Two entry points are exposed:
//!
//! - [`bulk_hostnames_and_enqueue`] — raw primitive: bulk-resolve a set of IPs
//!   against the cache, enqueue cold misses once each (dedup via
//!   `HashSet<IpAddr>`), return the map.
//! - [`stamp_hostnames`] — slice-of-T convenience wrapper that calls the raw
//!   helper and then walks the slice applying matched hostnames via
//!   caller-supplied closures.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use crate::hostname::{hostnames_for, SessionId};
use crate::state::AppState;

/// Bulk-resolve `ips` from the hostname cache and enqueue cold misses.
///
/// - Deduplicates `ips` via a `HashSet<IpAddr>` before hitting the DB so N
///   DTOs carrying the same IP produce exactly one DB lookup.
/// - Calls [`hostnames_for`] with the deduplicated set.
/// - For every IP whose result is absent from the map (cold miss), calls
///   `state.hostname_resolver.enqueue(ip, session)` exactly once — again
///   deduped via the same `HashSet` so a repeated cold-miss IP enqueues
///   once regardless of how many items referenced it.
///
/// # Return contract
///
/// The returned `HashMap<IpAddr, Option<String>>` contains **only IPs with
/// a cache entry** (positive or negative). Cold-miss IPs are absent from
/// the map entirely. Callers must interpret a lookup on an input IP as:
///
/// - `None` from `map.get(&ip)` → **cold miss** (no cache row; a resolver
///   job has been enqueued for this IP).
/// - `Some(None)` → **negative-cached** (confirmed NXDOMAIN or equivalent;
///   do not render a hostname).
/// - `Some(Some(h))` → **positive-cached** (render `h`).
///
/// The map is keyed on the **caller's input shape** — `map.get(&requested_ip)`
/// works even if the same IP appeared multiple times in `ips`.
pub async fn bulk_hostnames_and_enqueue(
    state: &AppState,
    session: &SessionId,
    ips: &[IpAddr],
) -> sqlx::Result<HashMap<IpAddr, Option<String>>> {
    if ips.is_empty() {
        return Ok(HashMap::new());
    }

    // Dedup before the DB call so we don't issue redundant lookups.
    let unique: Vec<IpAddr> = {
        let mut seen = HashSet::new();
        ips.iter().copied().filter(|ip| seen.insert(*ip)).collect()
    };

    let cache = hostnames_for(&state.pool, &unique).await?;

    // Enqueue cold misses — one call per IP, deduplicated via the unique set.
    for ip in &unique {
        if !cache.contains_key(ip) {
            state.hostname_resolver.enqueue(*ip, session.clone()).await;
        }
    }

    // Expand the map back to cover every input IP (including duplicates),
    // so callers can look up by any of the original input addresses.
    let result: HashMap<IpAddr, Option<String>> = ips
        .iter()
        .filter_map(|ip| cache.get(ip).cloned().map(|v| (*ip, v)))
        .collect();

    Ok(result)
}

/// Stamp reverse-DNS hostnames onto a slice of items.
///
/// - `ip_of(item)` extracts the set of [`IpAddr`]s to look up from one item.
/// - `apply(item, map)` writes the matched hostname(s) back onto the item.
///
/// Internally delegates to [`bulk_hostnames_and_enqueue`] so all IPs across
/// the entire slice are resolved in a single DB round-trip and cold misses
/// are enqueued exactly once.
pub async fn stamp_hostnames<T, Ips, Apply>(
    state: &AppState,
    session: &SessionId,
    items: &mut [T],
    ip_of: Ips,
    mut apply: Apply,
) -> sqlx::Result<()>
where
    Ips: Fn(&T) -> Vec<IpAddr>,
    Apply: FnMut(&mut T, &HashMap<IpAddr, Option<String>>),
{
    if items.is_empty() {
        return Ok(());
    }

    let all_ips: Vec<IpAddr> = items.iter().flat_map(ip_of).collect();
    if all_ips.is_empty() {
        return Ok(());
    }

    let map = bulk_hostnames_and_enqueue(state, session, &all_ips).await?;

    for item in items.iter_mut() {
        apply(item, &map);
    }

    Ok(())
}

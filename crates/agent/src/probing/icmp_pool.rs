//! Shared `surge_ping::Client` pool. One Client per address family covers
//! every per-target ICMP pinger in the agent process, replacing the
//! previous one-Client-per-target pattern (O(N) raw sockets and
//! dispatcher tasks). Identifiers are allocated centrally to avoid the
//! random-`u16` collision risk between independent Clients.

use std::net::IpAddr;
use std::sync::atomic::{AtomicU16, Ordering};

use surge_ping::{Client, Config, PingIdentifier, ICMP};
use tokio::sync::OnceCell;

/// Process-wide pool. Construct once in `bootstrap`, hand
/// `Arc<IcmpClientPool>` to every supervisor.
///
/// `Client` ownership: the v4 client is built eagerly (every meshmon agent
/// has at least one v4 target in practice); the v6 client is built lazily
/// on first v6 target so an all-v4 deployment never opens a v6 raw socket.
///
/// Identifier allocation: each `pinger()` call grabs a unique non-zero
/// `u16`. surge-ping treats `PingIdentifier(0)` as a wildcard that
/// matches any reply, so two pingers both holding `0` would cross-
/// attribute each other's replies — we skip 0 on wrap.
pub struct IcmpClientPool {
    v4: Client,
    v6: OnceCell<Client>,
    next_id: AtomicU16,
}

impl IcmpClientPool {
    /// Build the pool. Eagerly creates the v4 Client (raw socket + dispatcher);
    /// requires `CAP_NET_RAW` on Linux.
    pub fn new() -> anyhow::Result<Self> {
        let v4 =
            Client::new(&Config::default()).map_err(|e| anyhow::anyhow!("icmp v4 client: {e}"))?;
        Ok(Self {
            v4,
            v6: OnceCell::new(),
            // Start at 1 so the first allocation is non-zero; on wrap we
            // re-fetch to skip 0 again.
            next_id: AtomicU16::new(1),
        })
    }

    /// Allocate a unique non-zero `PingIdentifier` and return a `Pinger`
    /// bound to `target_ip` from the matching address-family Client.
    /// v6 Client is built lazily on first v6 target; subsequent v6 calls
    /// reuse the same Client.
    pub async fn pinger(&self, target_ip: IpAddr) -> anyhow::Result<surge_ping::Pinger> {
        let identifier = PingIdentifier(self.allocate_id());
        let pinger = match target_ip {
            IpAddr::V4(_) => self.v4.pinger(target_ip, identifier).await,
            IpAddr::V6(_) => {
                let v6 = self
                    .v6
                    .get_or_try_init(|| async {
                        Client::new(&Config::builder().kind(ICMP::V6).build())
                            .map_err(|e| anyhow::anyhow!("icmp v6 client: {e}"))
                    })
                    .await?;
                v6.pinger(target_ip, identifier).await
            }
        };
        Ok(pinger)
    }

    /// Atomically allocate the next non-zero identifier. Loops on wrap
    /// until a non-zero value is fetched; in steady state this returns on
    /// the first iteration and at the wrap boundary it returns on the
    /// second. Multiple threads racing on the wrap each consume one extra
    /// slot, which is harmless at the agent's allocation volume.
    ///
    /// `Ordering::Relaxed` is sufficient because callers only consume the
    /// returned `u16`; there is no happens-before dependency with any
    /// other shared state.
    fn allocate_id(&self) -> u16 {
        loop {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            if id != 0 {
                return id;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_id_is_strictly_non_zero_across_wrap() {
        // Mirrors `IcmpClientPool::allocate_id` against a bare `AtomicU16`
        // so the test runs without `CAP_NET_RAW` (which `Client::new`
        // requires on Linux).
        let counter = AtomicU16::new(u16::MAX);
        let allocate = || loop {
            let id = counter.fetch_add(1, Ordering::Relaxed);
            if id != 0 {
                return id;
            }
        };

        // First call: counter starts at MAX, fetch_add returns MAX (non-zero
        // → returned immediately), counter wraps to 0.
        assert_eq!(allocate(), u16::MAX);
        // Second call: counter is 0; fetch_add returns 0 → loop; next
        // fetch_add returns 1.
        assert_eq!(allocate(), 1);
    }

    // Live raw-socket test, gated behind CAP_NET_RAW.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires CAP_NET_RAW"]
    async fn pool_pings_loopback_via_v4_client() {
        let pool = IcmpClientPool::new().expect("v4 client");
        let mut p = pool
            .pinger("127.0.0.1".parse().unwrap())
            .await
            .expect("pinger");
        p.timeout(std::time::Duration::from_secs(2));
        let r = p.ping(surge_ping::PingSequence(1), &[0u8; 8]).await;
        assert!(r.is_ok(), "loopback ping failed: {r:?}");
    }
}

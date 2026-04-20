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
/// Both v4 and v6 Clients are built lazily on first use of the matching
/// address family, so a pool can always be constructed without privileges
/// (handy for unit tests). Production bootstrap calls [`Self::preflight`]
/// to force v4 creation eagerly and surface a missing `CAP_NET_RAW`
/// before any supervisor spawns.
///
/// Identifier allocation: each `pinger()` call grabs a unique non-zero
/// `u16`. surge-ping treats `PingIdentifier(0)` as a wildcard that
/// matches any reply, so two pingers both holding `0` would cross-
/// attribute each other's replies — we skip 0 on wrap.
pub struct IcmpClientPool {
    v4: OnceCell<Client>,
    v6: OnceCell<Client>,
    next_id: AtomicU16,
}

impl Default for IcmpClientPool {
    fn default() -> Self {
        Self::new()
    }
}

impl IcmpClientPool {
    /// Build an empty pool. Sockets are not opened until the first
    /// matching `pinger()` call (or [`Self::preflight`]).
    pub fn new() -> Self {
        Self {
            v4: OnceCell::new(),
            v6: OnceCell::new(),
            // Start at 1 so the first allocation is non-zero; on wrap we
            // re-fetch to skip 0 again.
            next_id: AtomicU16::new(1),
        }
    }

    /// Force v4 Client creation now. Production bootstrap calls this so a
    /// missing `CAP_NET_RAW` aborts startup loudly, instead of letting
    /// every per-target probe fail individually later.
    pub async fn preflight(&self) -> anyhow::Result<()> {
        self.v4_client().await?;
        Ok(())
    }

    /// Allocate a unique non-zero `PingIdentifier` and return a `Pinger`
    /// bound to `target_ip` from the matching address-family Client.
    /// Each address family's Client is built lazily on first use;
    /// subsequent calls reuse the same Client.
    pub async fn pinger(&self, target_ip: IpAddr) -> anyhow::Result<surge_ping::Pinger> {
        let identifier = PingIdentifier(self.allocate_id());
        let pinger = match target_ip {
            IpAddr::V4(_) => self.v4_client().await?.pinger(target_ip, identifier).await,
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

    async fn v4_client(&self) -> anyhow::Result<&Client> {
        self.v4
            .get_or_try_init(|| async {
                Client::new(&Config::default()).map_err(|e| anyhow::anyhow!("icmp v4 client: {e}"))
            })
            .await
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

/// Probe for `surge-ping` raw-socket capability and cache the answer
/// process-wide. Returns `true` if `surge-ping::Client::new` succeeds —
/// Linux `CAP_NET_RAW`, root, or macOS (where `surge-ping` falls back
/// to `SOCK_DGRAM`). Test-only — production should call
/// [`IcmpClientPool::preflight`] and surface the actual error instead.
#[cfg(test)]
pub(crate) fn raw_socket_available() -> bool {
    use std::sync::OnceLock;
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| Client::new(&Config::default()).is_ok())
}

/// Probe for an actual `SOCK_RAW` ICMP socket. Stricter than
/// [`raw_socket_available`] because it never falls back to `SOCK_DGRAM`,
/// matching what `trippy-core` requires. macOS without root fails this
/// even though [`raw_socket_available`] passes.
#[cfg(test)]
pub(crate) fn raw_ip_socket_available() -> bool {
    use socket2::{Domain, Protocol as SockProto, Socket, Type};
    use std::sync::OnceLock;
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| Socket::new(Domain::IPV4, Type::RAW, Some(SockProto::ICMPV4)).is_ok())
}

/// Skip the current test with an `eprintln!` notice when a `surge-ping`
/// pinger can't be opened. Use at the top of `#[tokio::test]` bodies
/// that exercise real ICMP echo probing — keeps coverage on dev boxes
/// while staying green on unprivileged CI.
#[cfg(test)]
macro_rules! skip_unless_raw_icmp {
    () => {
        if !$crate::probing::icmp_pool::raw_socket_available() {
            eprintln!(
                "skipping {}: raw ICMP socket bind requires CAP_NET_RAW (or macOS SOCK_DGRAM)",
                module_path!(),
            );
            return;
        }
    };
}
#[cfg(test)]
pub(crate) use skip_unless_raw_icmp;

/// Skip the current test when an actual `SOCK_RAW` ICMP socket can't
/// be opened. Trippy needs raw mode (no `SOCK_DGRAM` fallback), so this
/// is the right gate for any test that drives `trippy-core`.
#[cfg(test)]
macro_rules! skip_unless_raw_ip_socket {
    () => {
        if !$crate::probing::icmp_pool::raw_ip_socket_available() {
            eprintln!(
                "skipping {}: SOCK_RAW ICMP requires CAP_NET_RAW on Linux / root on macOS",
                module_path!(),
            );
            return;
        }
    };
}
#[cfg(test)]
pub(crate) use skip_unless_raw_ip_socket;

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

    #[test]
    fn new_does_not_open_a_socket() {
        // Construction must succeed without any privilege so unit tests
        // that wrap the pool can run on unprivileged CI runners. Real
        // socket creation is deferred until `preflight()` or `pinger()`.
        let _pool = IcmpClientPool::new();
    }

    // Live raw-socket test. Self-skips when the kernel denies the bind
    // so unprivileged CI runs stay green without a permanent #[ignore].
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pool_pings_loopback_via_v4_client() {
        skip_unless_raw_icmp!();
        let pool = IcmpClientPool::new();
        pool.preflight().await.expect("v4 client");
        let mut p = pool
            .pinger("127.0.0.1".parse().unwrap())
            .await
            .expect("pinger");
        p.timeout(std::time::Duration::from_secs(2));
        let r = p.ping(surge_ping::PingSequence(1), &[0u8; 8]).await;
        assert!(r.is_ok(), "loopback ping failed: {r:?}");
    }
}

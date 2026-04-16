//! Shared `reqwest::Client` factory for the proxies.

use reqwest::Client;
use std::sync::OnceLock;
use std::time::Duration;

static PROXY_CLIENT: OnceLock<Client> = OnceLock::new();

/// Return the process-shared proxy client, constructing it on first call.
///
/// The client is configured with conservative timeouts suitable for
/// proxying requests to Alertmanager and VictoriaMetrics:
///
/// - 2 s connect timeout — fail fast when the upstream is unreachable.
/// - 10 s overall timeout — generous enough for large VM query results.
/// - Custom `User-Agent` so upstream access logs can identify meshmon traffic.
pub fn proxy_client() -> &'static Client {
    PROXY_CLIENT.get_or_init(|| {
        Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(10))
            .user_agent(concat!("meshmon-service/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("failed to build reqwest proxy client — check TLS configuration")
    })
}

//! Tonic service implementation for the five agent RPCs.

use crate::ingestion::validator::{validate_metrics, validate_snapshot};
use crate::state::AppState;
use meshmon_protocol::TunnelFrame;
use meshmon_protocol::{
    ip as proto_ip, AgentApi, ConfigResponse, GetConfigRequest, GetTargetsRequest, MetricsBatch,
    PushMetricsResponse, PushRouteSnapshotResponse, RegisterRequest, RegisterResponse,
    RouteSnapshotRequest, Target as PbTarget, TargetsResponse,
};
use meshmon_protocol::{
    DiffDetection as PbDiffDetection, PathHealthThresholds as PbPathHealthThresholds,
    ProtocolThresholds as PbProtocolThresholds, RateEntry as PbRateEntry, Windows as PbWindows,
};
use std::net::IpAddr;
use tonic::{Request, Response, Status};

/// Resolve the caller's client IP from a tonic request.
///
/// `tonic::Request::remote_addr()` only knows `TcpConnectInfo`. The
/// in-process test harness inserts a bare `SocketAddr` directly (via
/// the `Connected` impl on `StreamWithPeer`), and production inserts
/// `axum::extract::ConnectInfo<SocketAddr>`. We try all three.
///
/// When `trust_forwarded` is `true`, an `X-Forwarded-For` or RFC 7239
/// `Forwarded` metadata header takes precedence over the raw peer
/// address — mirroring the behaviour of REST routes so that operators
/// who terminate TLS at a trusted proxy see a consistent client IP
/// across gRPC and HTTP. The caller is responsible for only enabling
/// the flag when the proxy is actually trusted.
fn resolve_peer_ip<T>(request: &Request<T>, trust_forwarded: bool) -> Option<IpAddr> {
    let from_transport = || -> Option<IpAddr> {
        // 1. tonic native (TcpConnectInfo — real TcpListener)
        request
            .remote_addr()
            // 2. bare SocketAddr (in-process StreamWithPeer harness)
            .or_else(|| request.extensions().get::<std::net::SocketAddr>().copied())
            // 3. axum ConnectInfo (production hyper integration path)
            .or_else(|| {
                request
                    .extensions()
                    .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                    .map(|ci| ci.0)
            })
            .map(|a| a.ip())
    };

    if trust_forwarded {
        // Check metadata first (proxies inject XFF or RFC 7239
        // `Forwarded`); fall back to the raw peer addr from the
        // transport. Shared helpers keep gRPC and REST in sync on
        // which header shapes they honor.
        request
            .metadata()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(crate::http::auth::parse_xff_client_ip)
            .or_else(|| {
                request
                    .metadata()
                    .get("forwarded")
                    .and_then(|v| v.to_str().ok())
                    .and_then(crate::http::auth::parse_forwarded_client_ip)
            })
            .or_else(from_transport)
    } else {
        from_transport()
    }
}

/// Concrete implementation of the `AgentApi` tonic service.
///
/// Each RPC body is a stub returning [`Status::unimplemented`]; the real
/// implementations arrive in Tasks 10–12, 15–16.
pub struct AgentApiImpl {
    state: AppState,
}

impl AgentApiImpl {
    /// Construct a new impl from the shared application state.
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// Borrow the shared application state.
    pub fn state(&self) -> &AppState {
        &self.state
    }
}

#[tonic::async_trait]
impl AgentApi for AgentApiImpl {
    type OpenTunnelStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<TunnelFrame, Status>> + Send + 'static>,
    >;

    #[tracing::instrument(skip_all, fields(source_id = tracing::field::Empty))]
    async fn open_tunnel(
        &self,
        request: Request<tonic::Streaming<TunnelFrame>>,
    ) -> Result<Response<Self::OpenTunnelStream>, Status> {
        // 1. Pull + validate source-id metadata header.
        let source_id = request
            .metadata()
            .get("x-meshmon-source-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .ok_or_else(|| Status::invalid_argument("x-meshmon-source-id metadata required"))?;
        if source_id.trim().is_empty() {
            return Err(Status::invalid_argument(
                "x-meshmon-source-id must not be empty",
            ));
        }
        tracing::Span::current().record("source_id", tracing::field::display(&source_id));

        // 2. Resolve the caller's peer IP. Bearer auth gated the RPC
        // upstream, but the shared-token model means any authenticated
        // agent could otherwise pass another agent's id in the
        // `x-meshmon-source-id` header and hijack that agent's tunnel
        // slot (`TunnelManager::accept` replaces the existing entry
        // and cancels its driver, severing the legitimate connection).
        // This handler binds the caller's peer IP to the IP registered
        // for `source_id`, mirroring `register`. When
        // `trust_forwarded_headers` is set, a proxy-supplied
        // `X-Forwarded-For` or RFC 7239 `Forwarded` header overrides
        // the raw transport peer — the operator is responsible for
        // only enabling that flag in front of trusted proxies.
        let trust_forwarded = self.state.config().service.trust_forwarded_headers;
        let peer_ip = resolve_peer_ip(&request, trust_forwarded)
            .ok_or_else(|| Status::invalid_argument("no client IP on request"))?;

        // 3. Validate against the live registry — unknown agents are
        // rejected, and the caller's peer IP must match the IP
        // registered for `source_id` (loopback exempt, mirroring
        // `register`'s loopback behaviour for developer-loop flows).
        let registered_ip = {
            let snap = self.state.registry.snapshot();
            let agent = snap
                .get(&source_id)
                .ok_or_else(|| Status::permission_denied("unknown source agent"))?;
            agent.ip.ip()
        };
        if !peer_ip.is_loopback() && peer_ip != registered_ip {
            tracing::warn!(
                %peer_ip, %registered_ip, source_id = %source_id,
                "open_tunnel: source agent IP does not match connection IP",
            );
            return Err(Status::permission_denied(
                "source agent IP does not match connection IP",
            ));
        }

        // 4. Hand the inbound stream off to the tunnel manager. Its detached
        // driver task calls `unregister` on session end. Tear-down is driven
        // by the manager's per-entry cancel token (fired by `close_all()` or
        // a same-source reconnect) — no outer cancel is needed here.
        let incoming = request.into_inner();
        let stream = self
            .state
            .tunnel_manager
            .clone()
            .accept(source_id, incoming)
            .await
            .map_err(|e| Status::unavailable(format!("tunnel setup failed: {e}")))?;

        Ok(Response::new(Box::pin(stream) as Self::OpenTunnelStream))
    }

    #[tracing::instrument(skip_all, fields(agent_id = tracing::field::Empty))]
    async fn register(
        &self,
        request: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        // Step 1: connection IP (preserved by grpc_harness::StreamWithPeer
        // in tests; preserved by main.rs `auto::Builder` loop in production).
        // The extractor handles the three transport shapes tonic / axum /
        // the in-process harness each produce; see `resolve_peer_ip` for
        // the precedence details.
        let trust_forwarded = self.state.config().service.trust_forwarded_headers;
        let peer_ip = resolve_peer_ip(&request, trust_forwarded)
            .ok_or_else(|| Status::invalid_argument("no client IP on request"))?;

        let req = request.into_inner();
        tracing::Span::current().record("agent_id", tracing::field::display(&req.id));

        // Step 2: payload validation.
        if req.id.trim().is_empty() || req.display_name.trim().is_empty() {
            return Err(Status::invalid_argument("id and display_name required"));
        }
        // Reject non-finite or out-of-range coordinates before they enter
        // the DB — `ip_catalogue.latitude`/`longitude` are `DOUBLE PRECISION`
        // and happily store NaN/Inf, which would then leak through
        // `GetTargets` and break clients that assume real-world ranges.
        if !req.lat.is_finite() || !(-90.0..=90.0).contains(&req.lat) {
            return Err(Status::invalid_argument(format!(
                "lat {} out of range [-90, 90]",
                req.lat
            )));
        }
        if !req.lon.is_finite() || !(-180.0..=180.0).contains(&req.lon) {
            return Err(Status::invalid_argument(format!(
                "lon {} out of range [-180, 180]",
                req.lon
            )));
        }
        // Probe ports are wire-encoded as `uint32`, but the valid TCP/UDP port
        // range is 1-65535. Zero is rejected because the DB column has a
        // CHECK constraint for [1, 65535]; `> 65535` is rejected because
        // `uint32` on the wire allows out-of-range values even though the
        // logical type is u16.
        if req.tcp_probe_port == 0 || req.tcp_probe_port > 65535 {
            return Err(Status::invalid_argument(format!(
                "tcp_probe_port {} out of range [1, 65535]",
                req.tcp_probe_port
            )));
        }
        if req.udp_probe_port == 0 || req.udp_probe_port > 65535 {
            return Err(Status::invalid_argument(format!(
                "udp_probe_port {} out of range [1, 65535]",
                req.udp_probe_port
            )));
        }
        let claimed_ip = proto_ip::to_ipaddr(&req.ip)
            .map_err(|e| Status::invalid_argument(format!("invalid ip bytes: {e}")))?;

        // Step 3: claimed IP must match connection IP (loopback exempt).
        if !peer_ip.is_loopback() && peer_ip != claimed_ip {
            tracing::warn!(
                %peer_ip, %claimed_ip, agent_id = %req.id,
                "register: claimed IP does not match connection IP",
            );
            return Err(Status::permission_denied(
                "claimed IP does not match connection IP",
            ));
        }

        // Step 4: same-ID different-IP preflight.
        let claimed_ip_net = sqlx::types::ipnetwork::IpNetwork::from(claimed_ip);
        let existing = sqlx::query!(
            r#"SELECT ip as "ip: sqlx::types::ipnetwork::IpNetwork"
               FROM agents WHERE id = $1"#,
            req.id
        )
        .fetch_optional(&self.state.pool)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "register: preflight select failed");
            Status::internal("register preflight failed")
        })?;
        if let Some(row) = existing {
            if row.ip.ip() != claimed_ip {
                tracing::warn!(
                    agent_id = %req.id,
                    existing_ip = %row.ip.ip(),
                    claimed_ip = %claimed_ip,
                    "register: id already registered with a different IP",
                );
                return Err(Status::already_exists(
                    "id already registered with a different IP",
                ));
            }
        }

        // Step 5: atomic upsert with IP guard, run inside a transaction
        // shared with the catalogue sync below (step 6). Bundling both
        // writes means a mid-sequence failure (e.g. catalogue DDL
        // temporarily unavailable or transient DB error on the second
        // query) rolls back the `agents` insert too — otherwise the
        // client sees INTERNAL while the agent row is already committed,
        // producing a partial-state view that a simple retry does not
        // reconcile against the catalogue.
        //
        // The `WHERE agents.ip = EXCLUDED.ip` clause on the conflict
        // branch closes the race where two concurrent Register calls
        // for the same `id` with different `ip` both pass the preflight
        // (both see `None`) — PostgreSQL serializes the conflict, and
        // the second caller's WHERE predicate evaluates against the
        // already-inserted row's `ip`, which now differs. In that case
        // the UPDATE is skipped, RETURNING yields zero rows, and we
        // surface the same ALREADY_EXISTS status as the preflight path.
        let location = (!req.location.is_empty()).then(|| req.location.clone());
        let agent_version = (!req.agent_version.is_empty()).then(|| req.agent_version.clone());
        // Validated to be in [1, 65535] above; narrow to the DB's i32 column.
        let tcp_port_i32 = req.tcp_probe_port as i32;
        let udp_port_i32 = req.udp_probe_port as i32;
        let mut tx = self.state.pool.begin().await.map_err(|e| {
            tracing::error!(error = %e, "register: open transaction failed");
            Status::internal("register: open transaction failed")
        })?;
        let upsert_row = sqlx::query!(
            r#"INSERT INTO agents
                  (id, display_name, location, ip, agent_version,
                   tcp_probe_port, udp_probe_port,
                   registered_at, last_seen_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())
               ON CONFLICT (id) DO UPDATE SET
                   display_name   = EXCLUDED.display_name,
                   location       = EXCLUDED.location,
                   agent_version  = EXCLUDED.agent_version,
                   tcp_probe_port = EXCLUDED.tcp_probe_port,
                   udp_probe_port = EXCLUDED.udp_probe_port,
                   last_seen_at   = NOW()
                 WHERE agents.ip = EXCLUDED.ip
               RETURNING id"#,
            req.id,
            req.display_name,
            location,
            claimed_ip_net,
            agent_version,
            tcp_port_i32,
            udp_port_i32,
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "register upsert failed");
            Status::internal("register upsert failed")
        })?;
        if upsert_row.is_none() {
            // Drop the transaction without committing; the agents upsert
            // above had no effect (UPDATE ... WHERE evaluated false), so
            // there is nothing to roll back but we must not leave the
            // connection holding an open BEGIN.
            let _ = tx.rollback().await;
            tracing::warn!(
                agent_id = %req.id,
                claimed_ip = %claimed_ip,
                "register: atomic conflict guard fired; id already registered with a different IP",
            );
            return Err(Status::already_exists(
                "id already registered with a different IP",
            ));
        }

        // Step 6: ensure the catalogue carries this agent's geo and marks
        // Latitude/Longitude as operator-edited so enrichment providers
        // never overwrite the agent's self-report. Runs against the same
        // transaction as the agents upsert; if this fails, the rollback
        // below also discards the `agents` write. SSE publish + enrichment
        // enqueue happen only after the transaction commits, so clients
        // never see a row that the DB will ultimately discard.
        let catalogue_entry = match crate::catalogue::repo::ensure_from_agent(
            &mut *tx,
            claimed_ip,
            req.lat,
            req.lon,
        )
        .await
        {
            Ok(entry) => entry,
            Err(e) => {
                let _ = tx.rollback().await;
                tracing::error!(
                    error = %e,
                    agent_id = %req.id,
                    claimed_ip = %claimed_ip,
                    "register: ip_catalogue ensure_from_agent failed",
                );
                return Err(Status::internal("register catalogue sync failed"));
            }
        };
        tx.commit().await.map_err(|e| {
            tracing::error!(error = %e, "register: commit transaction failed");
            Status::internal("register: commit transaction failed")
        })?;
        self.state
            .catalogue_broker
            .publish(crate::catalogue::events::CatalogueEvent::Updated {
                id: catalogue_entry.id,
            });
        if catalogue_entry.enrichment_status == crate::catalogue::model::EnrichmentStatus::Pending {
            // Bounded queue: a `false` return means the runner is busy
            // and the row will be picked up by the sweep instead — the
            // same safety net the paste path relies on.
            let _ = self.state.enrichment_queue.enqueue(catalogue_entry.id);
        }

        // Step 7: synchronous registry refresh so the next push sees it.
        if let Err(e) = self.state.registry.force_refresh().await {
            tracing::warn!(
                error = %e,
                "register: registry force_refresh failed; periodic refresh will catch up",
            );
        }

        Ok(Response::new(RegisterResponse::default()))
    }

    #[tracing::instrument(skip_all, fields(source_id = tracing::field::Empty))]
    async fn push_metrics(
        &self,
        request: Request<MetricsBatch>,
    ) -> Result<Response<PushMetricsResponse>, Status> {
        let batch = request.into_inner();
        tracing::Span::current().record("source_id", tracing::field::display(&batch.source_id));

        // Validate payload before authorization lookup so malformed
        // batches surface as INVALID_ARGUMENT rather than leaking through
        // the registry check as PERMISSION_DENIED (e.g. empty source_id,
        // missing target_id, bad failure_rate).
        let validated = validate_metrics(batch).map_err(|e| {
            tracing::debug!(error = %e, "metrics batch rejected by validator");
            Status::invalid_argument(e.to_string())
        })?;
        if self
            .state
            .registry
            .snapshot()
            .get(&validated.source_id)
            .is_none()
        {
            return Err(Status::permission_denied("unknown source agent"));
        }
        self.state.ingestion.push_metrics(validated);
        Ok(Response::new(PushMetricsResponse::default()))
    }

    #[tracing::instrument(
        skip_all,
        fields(source_id = tracing::field::Empty, target_id = tracing::field::Empty)
    )]
    async fn push_route_snapshot(
        &self,
        request: Request<RouteSnapshotRequest>,
    ) -> Result<Response<PushRouteSnapshotResponse>, Status> {
        let req = request.into_inner();
        tracing::Span::current().record("source_id", tracing::field::display(&req.source_id));
        tracing::Span::current().record("target_id", tracing::field::display(&req.target_id));

        // Validate payload before authorization lookup; see push_metrics
        // for rationale.
        let validated = validate_snapshot(req).map_err(|e| {
            tracing::debug!(error = %e, "route snapshot rejected by validator");
            Status::invalid_argument(e.to_string())
        })?;
        if self
            .state
            .registry
            .snapshot()
            .get(&validated.source_id)
            .is_none()
        {
            return Err(Status::permission_denied("unknown source agent"));
        }
        self.state.ingestion.push_snapshot(validated);
        Ok(Response::new(PushRouteSnapshotResponse::default()))
    }

    async fn get_config(
        &self,
        _request: Request<GetConfigRequest>,
    ) -> Result<Response<ConfigResponse>, Status> {
        let cfg = self.state.config();
        Ok(Response::new(probing_to_config_response(&cfg.probing)))
    }

    async fn get_targets(
        &self,
        request: Request<GetTargetsRequest>,
    ) -> Result<Response<TargetsResponse>, Status> {
        let req = request.into_inner();
        if req.source_id.trim().is_empty() {
            return Err(Status::invalid_argument("source_id required"));
        }
        let snap = self.state.registry.snapshot();
        let active = snap.active_targets(&req.source_id, self.state.registry.active_window());
        let targets = active
            .into_iter()
            .map(|a| PbTarget {
                id: a.id,
                ip: match a.ip.ip() {
                    std::net::IpAddr::V4(v) => v.octets().to_vec().into(),
                    std::net::IpAddr::V6(v) => v.octets().to_vec().into(),
                },
                display_name: a.display_name,
                location: a.location.unwrap_or_default(),
                lat: a.latitude.unwrap_or(0.0),
                lon: a.longitude.unwrap_or(0.0),
                tcp_probe_port: u32::from(a.tcp_probe_port),
                udp_probe_port: u32::from(a.udp_probe_port),
            })
            .collect();
        Ok(Response::new(TargetsResponse { targets }))
    }
}

fn probing_to_config_response(p: &crate::probing::ProbingSection) -> ConfigResponse {
    ConfigResponse {
        enabled_protocols: p.enabled_protocols.iter().map(|pp| *pp as i32).collect(),
        priority: p.priority.iter().map(|pp| *pp as i32).collect(),
        rates: p
            .rates
            .iter()
            .map(|r| PbRateEntry {
                primary: r.primary as i32,
                health: r.health as i32,
                icmp_pps: r.icmp_pps,
                tcp_pps: r.tcp_pps,
                udp_pps: r.udp_pps,
            })
            .collect(),
        icmp_thresholds: Some(PbProtocolThresholds {
            unhealthy_trigger_pct: p.icmp_thresholds.unhealthy_trigger_pct,
            healthy_recovery_pct: p.icmp_thresholds.healthy_recovery_pct,
            unhealthy_hysteresis_sec: p.icmp_thresholds.unhealthy_hysteresis_sec,
            healthy_hysteresis_sec: p.icmp_thresholds.healthy_hysteresis_sec,
        }),
        tcp_thresholds: Some(PbProtocolThresholds {
            unhealthy_trigger_pct: p.tcp_thresholds.unhealthy_trigger_pct,
            healthy_recovery_pct: p.tcp_thresholds.healthy_recovery_pct,
            unhealthy_hysteresis_sec: p.tcp_thresholds.unhealthy_hysteresis_sec,
            healthy_hysteresis_sec: p.tcp_thresholds.healthy_hysteresis_sec,
        }),
        udp_thresholds: Some(PbProtocolThresholds {
            unhealthy_trigger_pct: p.udp_thresholds.unhealthy_trigger_pct,
            healthy_recovery_pct: p.udp_thresholds.healthy_recovery_pct,
            unhealthy_hysteresis_sec: p.udp_thresholds.unhealthy_hysteresis_sec,
            healthy_hysteresis_sec: p.udp_thresholds.healthy_hysteresis_sec,
        }),
        windows: Some(PbWindows {
            primary_sec: p.windows.primary_sec,
            diversity_sec: p.windows.diversity_sec,
        }),
        diff_detection: Some(PbDiffDetection {
            new_ip_min_freq: p.diff_detection.new_ip_min_freq,
            missing_ip_max_freq: p.diff_detection.missing_ip_max_freq,
            hop_count_change: p.diff_detection.hop_count_change,
            rtt_shift_frac: p.diff_detection.rtt_shift_frac,
        }),
        path_health_thresholds: Some(PbPathHealthThresholds {
            degraded_trigger_pct: p.path_health_thresholds.degraded_trigger_pct,
            degraded_trigger_sec: p.path_health_thresholds.degraded_trigger_sec,
            degraded_min_samples: p.path_health_thresholds.degraded_min_samples,
            normal_recovery_pct: p.path_health_thresholds.normal_recovery_pct,
            normal_recovery_sec: p.path_health_thresholds.normal_recovery_sec,
        }),
        udp_probe_secret: p.udp_probe_secret.to_vec().into(),
        udp_probe_previous_secret: p
            .udp_probe_previous_secret
            .map(|s| s.to_vec().into())
            .unwrap_or_default(),
    }
}

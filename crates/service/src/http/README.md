# `crates/service/src/http/`

HTTP handlers that hang off the user-API router. Each module owns one
cohesive surface; route registration lives in the crate's top-level
`router` wiring and the OpenAPI manifest in [`openapi.rs`](openapi.rs).

Session authentication is enforced at the middleware layer — handlers
in this directory do **not** take an `AuthSession` extractor. A
handler placed here is assumed to require an authenticated operator.

## Files

| File | Role |
|---|---|
| `mod.rs` | Re-exports the submodules below. |
| `openapi.rs` | `utoipa` `ApiDoc` spec and `routes!` registration. |
| `session.rs` | `/api/session/*` — login, logout, current session. |
| `auth.rs` | Session middleware + extractors. |
| `user_api.rs` | Wiring layer that composes the authenticated router. |
| `health.rs` | `/healthz` liveness. |
| `history.rs` | `/api/history/*` + `/api/campaigns/{id}/measurements` (see "History module" below). |
| `path_overview.rs` | `/api/paths/*` list and detail surfaces. |
| `alerts_proxy.rs` | `/api/alerts/*` — proxies to the embedded Alertmanager. |
| `alertmanager_proxy.rs` | Low-level Alertmanager proxy transport. |
| `grafana_proxy.rs` | `/api/grafana/*` — proxies to the embedded Grafana, inheriting the auth-proxy headers. |
| `metrics_proxy.rs` | `/api/metrics/*` — VictoriaMetrics proxy. |
| `metrics_auth.rs` | Auth glue for the metrics proxy. |
| `http_client.rs` | Shared `reqwest::Client` + proxy helpers. |
| `proxy_common.rs` | Shared proxy request / response shaping. |

## History module

[`history.rs`](history.rs) owns four read-only endpoints that feed the
operator UI. All four reuse the existing
`measurements_reuse_idx (source_agent_id, destination_ip, protocol,
probe_count DESC, measured_at DESC)` — no index is added.

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/api/history/sources` | Agents that have produced at least one `measurements` row, alphabetised by catalogue display name. |
| `GET` | `/api/history/destinations` | Destinations reachable from `?source=<agent_id>`, optionally narrowed by `?q=<partial>`. Catalogue join is `LEFT JOIN` so deleted rows surface as raw IPs with null metadata. |
| `GET` | `/api/history/measurements` | Measurement rows + inline `mtr_traces.hops` for a `(source, destination)` over an optional protocol list and time window. Hard-capped at **5 000 rows**; the frontend shows an explicit notice when the cap is hit. |
| `GET` | `/api/campaigns/{id}/measurements` | Raw-tab feed: `campaign_pairs` LEFT JOIN `measurements` LEFT JOIN `mtr_traces`. Keyset pagination on `(measured_at DESC NULLS LAST, pair_id DESC)`; cursor is base64-encoded JSON. Pending / dispatched pairs stay visible and accumulate at the bottom of the first page; they are unreachable via cursor, which is acceptable — the filter chips funnel pending-only views through `?resolution_state=pending`. A `?measurement_id=` shortcut resolves one row for the DrilldownDrawer's MTR lookup. |

### DTOs

- `HistorySourceDto` — `{ source_agent_id, display_name }`.
- `HistoryDestinationDto` — `{ destination_ip, display_name, city?, country_code?, asn?, is_mesh_member }`.
- `HistoryMeasurementDto` — measurement aggregates + inline MTR hops.
- `CampaignMeasurementDto` + `CampaignMeasurementsPage` — Raw tab row + paged envelope.

### Errors

All four fail to `500 { "error": "internal" }` on database error. The
measurements endpoint additionally returns:

- `400 { "error": "invalid_destination_ip" }` on a malformed `destination=` IP.
- `400 { "error": "invalid_protocols" }` on an unrecognised protocol token in the `protocols=` CSV.

# `catalogue`

The authoritative per-IP registry. Every IP meshmon knows about —
operator-added or agent-derived — lives in `ip_catalogue`, with
per-row enrichment status, operator-overridable fields, and an SSE
event stream for live UI updates.

## Files

| File | Role |
|---|---|
| `mod.rs` | Module surface; re-exports the submodules below. |
| `model.rs` | `CatalogueEntry`, the `Field` enum, and the PascalCase encoding used in `operator_edited_fields`. |
| `parse.rs` | Paste tokeniser. Accepts bare IPv4 / IPv6 and `/32` or `/128` CIDRs; rejects wider prefixes. |
| `repo.rs` | sqlx queries: `insert_many`, `find_by_id`, `list`, `patch`, `delete`, `facets`, `ensure_from_agent`, `mark_enrichment_start`, `apply_enrichment_result`. |
| `events.rs` | `CatalogueBroker` (wraps `tokio::sync::broadcast`, capacity 512) and the `CatalogueEvent` wire shape. |
| `sse.rs` | `GET /api/catalogue/stream` handler; serialises events and injects synthetic `lag` frames on receiver overflow. |
| `dto.rs` | `utoipa`-annotated request / response bodies. |
| `handlers.rs` | HTTP handlers for paste, list, get, patch, delete, single / bulk re-enrich, and cached facets. |
| `facets.rs` | `FacetsCache`: 30-second TTL wrapper around `repo::facets` with single-flight refresh. |

## How new IPs land in the DB

Two entry points create rows:

- **Operator paste** — `POST /api/catalogue` → `handlers::paste` →
  `parse::parse_ip_tokens` → `repo::insert_many` → for each newly-
  created id, `AppState::enrichment_queue.enqueue(id)`.
- **Agent Register** — `AgentApi::Register` →
  `repo::ensure_from_agent(pool, ip, lat, lon)` → the row is created
  (or latitude / longitude is refreshed) with `operator_edited_fields`
  union-merged to include `Latitude` and `Longitude`. The runner sweep
  later picks the `pending` row up for enrichment of the unlocked
  fields.

The enrichment runner is the single persistence point for provider
output — handlers never write enrichment columns directly.

## When SSE is silent

If the UI stops receiving events, work through the chain in order:

1. **Broker capacity warnings.** Grep the service logs for broadcast
   overflow; sustained overflow means publishers are outrunning
   subscribers and a `lag` frame should have appeared on the client.
2. **Route wiring.** Confirm `/api/catalogue/stream` is registered in
   `crate::http::openapi::api_router` (alongside the other catalogue
   routes). A missing registration returns 404, not a silent stream.
3. **Client resync.** The handler emits `{"kind":"lag","missed":N}`
   on receiver overflow; clients that don't handle this frame will
   appear silent after a burst. The cure is a catalogue refetch,
   not a reconnect.

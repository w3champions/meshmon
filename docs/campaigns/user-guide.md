# Campaigns — User Guide

Operator workflow for the IP catalogue and the measurement-campaign
surface that builds on top of it. See [`architecture.md`](architecture.md)
for the developer reference.

## The IP catalogue

The **Catalogue** page holds every IP meshmon knows about — operator-
added entries and the rows auto-created for each meshmon agent. Each
row carries an IP, a display name, a structured location (city,
country, coordinates), an ASN, a network operator, an optional
website, and free-text notes.

## Adding IPs

Click **Add IPs**. A paste box accepts one IP per line or a comma- /
whitespace-separated batch — hundreds at a time.

- Accepted tokens: bare IPv4 / IPv6 (e.g. `1.1.1.1`, `2606:4700::1111`)
  and host-prefix CIDRs (`1.1.1.1/32`, `2606:4700::1111/128`).
- Rejected tokens: any wider CIDR (`10.0.0.0/24`, `2001:db8::/48`). The
  catalogue is a per-host registry, never a range store; the paste
  response lists each rejection with reason `cidr_not_allowed:/N` so
  the UI can surface inline errors.
- Invalid tokens are reported as `invalid_ip`.

Deduplication is automatic:

- Within a single paste, repeated IPs collapse to one accepted entry
  and the response reports the duplicate count.
- Across pastes, IPs already in the catalogue appear in the `existing`
  bucket — no second row is created.

Newly-inserted rows start with `enrichment_status = pending` and each
id is enqueued for the background enrichment pipeline. Fields fill in
live as providers respond; the page reacts to `enrichment_progress`
SSE events to flip the status badge without a refresh.

## Filtering

The filter rail accepts:

- **Country** — exact ISO country code match; multi-select.
- **ASN** — exact match; multi-select.
- **Network operator** — substring match.
- **IP prefix** — accepts any Postgres-parseable CIDR or bare IP.
- **Name search** — full-text search over display name, city, country
  name, and network operator.
- **Bounding box** — four-value `[minLat, minLon, maxLat, maxLon]`
  driven by the map's draw tool.

Filter facets (top countries, top ASNs, top cities, top networks) are
served from a cached snapshot with a 30-second TTL. Immediately after
a large batch of changes, the filter hints may show slightly stale
counts until the cache refreshes.

## Editing catalogue rows

Click a row to open the edit drawer. Every field is editable. Anything
you save is treated as authoritative — subsequent enrichment runs
leave that field alone.

Each edited field gains a **Revert to auto** link. Clicking it clears
your value *and* removes the lock, so the next enrichment pass
re-populates the column from the provider chain.

## Re-enrichment

The **Re-enrich** button on a row (returns 202 Accepted) enqueues a
fresh enrichment pass against the provider chain. Your manually-edited
fields stay put — only unlocked columns can change.

Bulk re-enrich on a selection enqueues the whole set in one call. Both
paths feed the same background runner; order is not guaranteed.

`ipgeolocation`'s free tier has a daily quota shared across the whole
deployment. Each re-enrich click uses one credit.

## Deleting

**Delete** removes the row immediately. The call is idempotent: a
delete against a missing id still returns success.

## Agents in the catalogue

Every meshmon agent creates or refreshes its catalogue entry on
register. The agent's self-reported latitude and longitude are locked
— enrichment providers never overwrite them, and the row is marked
with `source = agent_registration`. Everything else (city, country,
ASN, network operator) remains open for providers to populate, and
you can still edit any field manually.

## Live updates

The catalogue page maintains an SSE subscription to
`/api/catalogue/stream`. Every create, update, delete, and
enrichment-progress event is delivered with a `kind` discriminant so
the UI can update in place without polling.

If a client falls far behind the server (slow network, background
tab), the stream emits a synthetic `{"kind":"lag","missed":N}` frame
so the page knows to refetch rather than operate on a stale view.

---

# Measurement campaigns

A measurement campaign schedules probes between a chosen set of source
agents and destination IPs, records the results, and surfaces them for
analysis. The campaign composer UI is the operator's entry point; the
sections below cover the backend contract so operators can predict
what happens when they press each button.

## Creating a campaign

`POST /api/campaigns` creates a campaign in `draft` state. The
composer posts:

- `title` (required, non-blank) and optional `notes`.
- `protocol` — one of `icmp`, `tcp`, `udp`.
- `source_agent_ids` — list of agents that will probe.
- `destination_ips` — list of IP strings (v4 or v6); wider CIDRs are
  rejected at the catalogue layer, so the composer only ever feeds
  host-address strings.
- Optional overrides: `probe_count` (default 10), `probe_count_detail`
  (default 250), `timeout_ms` (default 2000), `probe_stagger_ms`
  (default 100), `loss_threshold_pct` (default 2.0), `stddev_weight`
  (default 1.0), `evaluation_mode` (default `optimization`).
- `force_measurement` — when `true`, the scheduler ignores the 24 h
  reuse cache for every pair in this campaign.

The backend seeds `campaign_pairs` with the `sources × destinations`
cross product inside the same transaction. The campaign stays in
`draft` until the operator presses **Start**; no probes are dispatched
while in `draft`.

### Size preview

`GET /api/campaigns/:id/preview-dispatch-count` returns
`{ total, reusable, fresh }` against the campaign's current pair set.
`total` is the full `sources × destinations` count, `reusable` is the
subset resolvable from the 24 h reuse window, and `fresh` is the
dispatch estimate (`total - reusable`).

The composer shows a confirm dialog when `total` exceeds
`[campaigns] size_warning_threshold` (default 1000). The threshold is
a soft warning — the backend does not enforce a hard cap.

## Starting a campaign

`POST /api/campaigns/:id/start` transitions `draft → running` and
stamps `started_at`. The scheduler picks the campaign up on its next
wake-up (ahead of the 500 ms tick thanks to the `campaign_state_changed`
NOTIFY fired by the trigger on `measurement_campaigns`).

Starting a campaign that is already `running`, `completed`, `stopped`,
or `evaluated` returns `409 illegal_state_transition`.

## Stopping a campaign

`POST /api/campaigns/:id/stop` transitions `running → stopped` and, in
the same transaction, flips every `pending` pair to `skipped` (with
`last_error = 'campaign_stopped'`). In-flight dispatched pairs drain
as-is — the dispatch-layer writer settles them into `succeeded`,
`unreachable`, or `skipped` as they land. The campaign stays in
`stopped` until you edit it.

Stopping a campaign that is not `running` returns
`409 illegal_state_transition`.

## Editing after completion

`POST /api/campaigns/:id/edit` applies a delta against a finished
campaign (`completed`, `stopped`, or `evaluated`) and transitions it
back to `running`. `started_at` is bumped so the scheduler treats the
re-run as a fresh activation.

The body carries three independent knobs:

- **`add_pairs`** — `[{ source_agent_id, destination_ip }]`. New pairs
  are inserted as `pending`. If a pair with the same `(source,
  destination)` already exists (for example, one that was `skipped`
  on a prior run), it is reset to `pending` with `attempt_count`,
  `last_error`, `measurement_id`, `dispatched_at`, and `settled_at`
  cleared.
- **`remove_pairs`** — `[{ source_agent_id, destination_ip }]`. Exact-
  match `DELETE`. Silent no-op when the pair does not exist.
- **`force_measurement`** — optional boolean:
  - Default (absent / `false`): only the delta pairs run. Existing
    terminal pairs keep their previous results.
  - `true`: the sticky `measurement_campaigns.force_measurement` flag
    flips to `TRUE`, and every non-delta pair currently in `reused`,
    `succeeded`, or `unreachable` is reset to `pending`. The whole
    campaign re-runs, and the 24 h reuse cache is ignored for the
    duration.

The edit is atomic — the state flip and the pair mutations commit
together — and holds a `FOR UPDATE` lock on the campaign row so a
concurrent `maybe_complete` or evaluation flip cannot race.

## Force-remeasure a single pair

`POST /api/campaigns/:id/force_pair` with
`{ source_agent_id, destination_ip }` resets one specific pair to
`pending` (clearing `attempt_count`, `last_error`, `measurement_id`,
`dispatched_at`, `settled_at`) and ensures the parent campaign is in
`running`. Valid from `running`, `completed`, `stopped`, or
`evaluated`.

Returns `404 not_found` when the `(source, destination)` pair does
not exist for the campaign.

## Listing pairs

`GET /api/campaigns/:id/pairs` returns the pair list with every
lifecycle column (`resolution_state`, `attempt_count`, `last_error`,
`measurement_id`, `dispatched_at`, `settled_at`). The `state` query
parameter accepts a comma-separated list of
`pair_resolution_state` values (e.g. `?state=pending,dispatched`) —
the repeat-key form (`?state=pending&state=dispatched`) is not
supported. `limit` defaults to 500 and is clamped to 5 000.

## The 24 h reuse window

The scheduler consults the `measurements` table before dispatching
each batch. For every pair, it looks up the most recent measurement
matching `(source_agent_id, destination_ip, protocol)` within the
last 24 hours. When multiple candidates exist, the one with the
highest `probe_count` wins; `measured_at DESC` is the tiebreaker.

Matched pairs flip to `reused` with `measurement_id` pointing at the
reused row — no probe is dispatched and the agent is not touched at
all. Unmatched pairs fall through to the normal dispatch path.

The 24-hour window is fixed. It is not a configuration knob, not
per-protocol, and not per-campaign. The only way to bypass reuse is
to set `force_measurement = true` on the campaign (either at create
time or via an edit delta); that flag skips the lookup entirely.

## Evaluation

The evaluation pass that produces `evaluated` results is covered by a
later subsystem. A `completed` campaign can be flipped to `evaluated`
once the evaluator ships; `evaluated` campaigns accept edit deltas and
round-trip through `running → completed → evaluated` again.

## See also

- [Architecture](architecture.md) — data model, scheduler, 24 h reuse,
  HTTP surface.
- [Runbook](../runbook.md) — operational response.

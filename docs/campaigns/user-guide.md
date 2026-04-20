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

### Default metadata

The **Add IPs** dialog includes an optional **Default metadata**
panel. Expand it to set display name, city, country, location,
website, and notes once for every IP in the paste. Blank fields are
ignored. Country (code + name) and Location (latitude + longitude)
travel as atomic pairs — the server rejects a half-supplied pair, so
the panel's country and location pickers always emit both halves
together.

Merge rules match the rest of the catalogue's lock model:

- New rows receive every supplied field, and each supplied field is
  added to `operator_edited_fields` so later enrichment runs skip it.
- Existing rows receive a field only if it is not already in
  `operator_edited_fields`. Paired fields are atomic: if either half
  of Location or Country is already locked, neither half is written.

The response's `skipped_summary` surfaces the aggregate — when
existing rows kept locked values, an inline notice inside the dialog
names the skipped fields so the operator sees what survived.

### Pick coordinates on a map

Latitude and longitude (both in the Default metadata panel and the
entry drawer's edit form) are set by clicking on a Leaflet map.
Click drops a marker, drag moves it, and the Clear button nulls it
back to the empty state. The selected coordinates show below the map
for verification, and the component respects the system's
reduced-motion preference.

## Filtering

The filter rail runs across the top of the catalogue view. Every
filter runs server-side, so the row counter always reflects the true
filter size — including every row the current page hasn't loaded yet.
Active filters compose with AND; multi-select values within a single
filter compose with OR.

| Filter | Behaviour |
|---|---|
| **Country** | Exact ISO country-code match; multi-select. |
| **ASN** | Exact match; multi-select. |
| **Network** | Substring match against the network-operator field. |
| **City** | Substring match against the city field; multi-select. |
| **Name** | Full-text search over display name, city, country name, and network operator. |
| **IP prefix** | Accepts any Postgres-parseable CIDR or bare IP. |
| **Map polygon** | A row matches when its geo-pin falls inside any drawn shape (OR across shapes). The server pre-filters by the shapes' union bounding box, then runs an exact point-in-polygon pass per returned row. |

Switching to **Map view** reveals the draw toolbar (rectangle, circle,
freehand polygon). Each shape you draw narrows the *table* view; the
filter clears when you remove every shape or click **Clear** in the
filter rail.

The map view itself stays shape-blind and city-blind on purpose: the
point of drawing shapes is to select *against* the catalogue's
existing geography, which is hard to do if the map is already
narrowed. Every other filter in the rail (country, ASN, network, IP
prefix, name) flows through to the map query so pins and clusters
reflect those choices.

Entries with no coordinates never appear on the map — their rows
still show up in the table.

### Paging and sort

The table uses keyset pagination. Each page holds up to 100 rows; the
**Load more** control walks the server-provided cursor until the full
result set has been fetched. Re-applying a filter or changing the
sort resets the cursor chain and the table rewinds to the first page.

Click any column header to sort by that column. Header clicks cycle
**off → ascending → descending → off**; only one column can drive the
sort at a time. Nullable columns place nulls last in both directions,
so rows with populated values always sort together ahead of the
`—` placeholders. The sort picks round-trip through the URL
(`?sort=<col>&dir=<asc|desc>`), so refreshing or sharing the URL
preserves the view.

When a shape filter is active, the row-count badge renders with a
"~" prefix: the server counts rows inside the shapes' bounding box,
then excludes point-in-polygon misses per page. Without shape
filters the count is exact.

### Map clustering

The map response adapts to the current viewport. Below 2000 rows the
server returns individual pins; above it, pre-aggregated cluster
bubbles sized by count. Clicking a cluster bubble opens a dialog
listing every catalogue entry inside that cell — the dialog uses its
own Load-more button to walk pages of 50 through the cursor chain.
Zooming in tightens the cluster cell size automatically (1° at mid
zoom, 0.05° once you're in on a city, 0.01° at street level), so
clusters peel into pins as the viewport narrows.

Filter facets (top countries, top ASNs, top cities, top networks) are
served from a cached snapshot with a 30-second TTL. Immediately after
a large batch of changes, the filter hints may show slightly stale
counts until the cache refreshes.

## Editing catalogue rows

Click any row to open the edit drawer. Every field is editable.
Anything you save is treated as authoritative — subsequent enrichment
runs leave that field alone.

Each operator-edited field shows a **Revert to auto** link next to its
label. Clicking it sends a `revert_to_auto` patch that NULLs the
column value *and* removes that field from the lock set on the server,
so the next enrichment pass re-populates the column from the provider
chain. Reverts take effect immediately; the drawer refreshes to the
server's response.

## Re-enrichment

The **Re-enrich** button on a row (or in the edit drawer) enqueues a
fresh enrichment pass against the provider chain. Operator-locked
fields stay put — only unlocked columns can change.

**Bulk re-enrich** on a selection (25+ rows) shows a confirmation
dialog that displays the approximate credit cost ("~N ipgeolocation
credits") before anything is sent. Confirm to enqueue the entire
selection in one call. Both paths feed the same background runner;
completion order is not guaranteed.

`ipgeolocation`'s free tier has a daily quota shared across the whole
deployment. Each enrichment pass uses one credit per IP.

## Deleting

**Delete** in the edit drawer removes the row immediately. The call is
idempotent: a delete against a missing id still returns success.

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

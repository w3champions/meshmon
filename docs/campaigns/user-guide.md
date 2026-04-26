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
  (default 100), `loss_threshold_ratio` (default 0.02), `stddev_weight`
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

## Reading results

Open the campaign in `/campaigns/:id`. The page splits into tabs;
the active tab rides in the URL as `?tab=` so refreshes and shared
links survive.

| Tab | What it answers | Modes |
|---|---|---|
| **Candidates** (default) | Which destinations qualify? | all |
| **Heatmap** | X × A latency matrix | `edge_candidate` only |
| **Pairs** | What happened to each baseline pair? | all |
| **Compare** | Re-aggregated candidate stats against a subset of agents | all |
| **Raw** | Every measurement attributed to the campaign, including in-flight detail work. | all |
| **Settings** | What knobs did the evaluator use? Re-evaluate here. | all |

### Candidates tab

A KPI strip at the top shows baseline pair count, qualifying
candidates, and average improvement. The candidate table below ranks
destinations by
`composite_score = (pairs_improved / baseline_pair_count) × avg_improvement_ms`.
Each row shows: rank, name, IP, city, ASN + network operator,
improved/total pairs, average improvement, and a loss chip (green
< 0.5%, yellow below threshold, red above). Columns are sortable by
clicking the header (round-tripped through the URL as `?cand_sort=…&cand_dir=…`);
the default is `composite_score desc`. Click a row to open the
drilldown drawer: per-pair direct-vs-transit RTT with green/red
improvement deltas (positive means transit is faster than direct), loss
per leg, and an inline `RouteTopology` view for any MTR trace linked to
the triple.

Mesh-member candidates (destinations that are themselves meshmon
agents) render with a "mesh member — no acquisition needed" badge.
They're useful context in `diversity` mode and automatically filter
out of `optimization` results.

Row actions on each candidate:

- **Force remeasure** — reset every pair belonging to the candidate to
  `pending` and re-run bypassing the reuse cache.
- **Detail this candidate** — dispatch detail measurements across the
  candidate's qualifying triples (the cost-preview dialog confirms
  before firing).

### Pairs tab

One row per baseline pair. Columns: source agent, destination,
resolution state, attempts, last error, last measurement timestamp.
The row-action menu offers **Force remeasure** and **Detail pair**. The
tab is the right place to chase a single misbehaving leg without the
candidate-ranking context.

### Raw tab

Every measurement row the service has attributed to the campaign,
including pending and dispatched pairs that have not settled yet.
Virtualised so long lists stay responsive. Filter chips at the top
narrow by `resolution_state`, protocol, and kind
(`campaign` / `detail_ping` / `detail_mtr`); chip selections round-trip
through the URL. Each row links to the historic pair view
(`/history/pair`) pre-filtered to the same `(source, destination)` so
the operator can compare campaign samples against the broader history.

### Settings tab

Shows the three evaluator knobs (`loss_threshold_ratio`, `stddev_weight`,
`evaluation_mode`) along with a **Re-evaluate** button. Only `completed`
and `evaluated` states enable Re-evaluate; it's hidden on `draft`,
`running`, and `stopped` campaigns.

Re-evaluate re-scores the campaign against every agent→agent
baseline it can find: first the active-probe measurements attributed
to the campaign, then — for any agent→agent pair the active probes
didn't cover — samples pulled from VictoriaMetrics continuous-mesh
data at evaluate time. Active-probe data wins when both sources cover
the same pair; VM-sourced rows never land in `measurements`, they only
feed the evaluator for that single call.

Each pair in the result carries a `direct_source` field
(`active_probe` | `vm_continuous`) so operators can tell whether a
given baseline came from the campaign's own probes or from the
continuous mesh.

Error responses:

- 422 `no_baseline_pairs` — both sources were empty for every
  agent→agent pair in the campaign's source/destination agent roster.
  When the deployment has no VictoriaMetrics configured
  (`[upstream] vm_url` unset), only the active-probe set is consulted
  before this error fires.
- 503 `vm_upstream` — `[upstream] vm_url` is configured but the VM
  query failed (unreachable, non-2xx, or malformed response).
  Retry-safe: no evaluation row is written.

### Switching modes

Use **Diversity** when you want to know every destination that beats
the direct path, regardless of what the mesh already provides —
useful for redundancy planning. Use **Optimization** (default) when
you want only destinations that beat every alternative the mesh
already has — useful for "should we acquire this server?" Use
**Edge candidate** when you want to measure how well a new IP (X)
connects to each source agent in the mesh — useful for evaluating
servers you are considering adding as leaf nodes.

## Evaluating new edge candidates

Edge candidate mode measures how well candidate IPs (X) reach each
source agent in the mesh, rather than scoring X as a transit between
two agents. The workflow below walks through a full campaign from setup
to results.

### 1. Open the Campaigns composer

Navigate to **/campaigns/new**.

### 2. Select Edge candidate as the evaluation mode

In the **Evaluation mode** selector, choose **Edge candidate**. The
composer updates the knob panel to show the edge-candidate-specific
parameters.

### 3. Pick the source agents

In the **Sources** picker, select the mesh agents that will probe the
candidates. These agents serve a dual role: they are both the probers
(they issue the measurements to each candidate IP) and the mesh agents
that the candidates are evaluated against (a candidate's coverage score
is the fraction of these agents it reaches under the latency threshold).

An explainer callout on the source picker describes this dual role.
When only one source agent is selected, a banner in the results view
notes that the evaluation reflects connectivity to a single agent.

### 4. Pick the candidate destinations

In the **Destinations** picker, select the candidate IPs (X) you want
to evaluate. These are the edge nodes under consideration — they do not
need to be existing mesh members. Use the catalogue filter rail or the
paste flow to add them.

### 5. Set the `useful_latency_ms` threshold

The **Useful latency** field is required. Enter the RTT (in ms) below
which a route from candidate X to a mesh agent is considered useful.
Candidates with more useful connections rank higher. A route whose RTT
exceeds this threshold is still measured and stored, but it does not
count toward `coverage_count`.

### 6. Optionally adjust `max_hops`

**Max hops** controls how many intermediate mesh agents may appear in a
route. Default is 2.

| Value | Allowed route shapes |
|---|---|
| 0 | Direct only (X → A) |
| 1 | Direct or one intermediate hop (X → M → A) |
| 2 | Direct, one-hop, or two-hop (X → M₁ → M₂ → A) |

Lower values run faster; higher values find more paths at the cost of
additional route enumeration.

### 7. Optionally adjust `vm_lookback_minutes`

**VM lookback** sets how far back (in minutes) the evaluator pulls
continuous-mesh baselines from VictoriaMetrics for agent→agent legs
that the campaign's own probes did not cover. Default is 15 minutes.
This knob is available in all modes; for edge_candidate it affects mesh
inter-agent legs used as intermediary hops.

### 8. Run and evaluate

Click **Start**. Once the campaign reaches **Completed**, the
evaluator runs automatically. You can also click **Re-evaluate** on
the **Settings** tab to re-score against the same measurements with
adjusted knobs.

### 9. Review results

The results page shows the following tabs for an edge-candidate
evaluation:

| Tab | Content |
|---|---|
| **Candidates** | Candidate rows ranked by `coverage_count` then `coverage_weighted_ping_ms`. Each row shows coverage count, weighted ping, mean RTT under threshold, and a route-mix breakdown (direct / 1-hop / 2-hop). |
| **Heatmap** | X × A latency matrix. Rows are destination agents (A), columns are candidate IPs (X). Each cell shows `best_route_ms` or "—" for unreachable pairs; color tiers are derived from `useful_latency_ms`. Available only for edge_candidate evaluations. |
| **Pairs** | Raw pair list showing each `(source_agent, destination_ip)` pair's resolution state and measurement outcome. |
| **Compare** | Re-aggregated candidate stats against a subset of source agents. Use the agent picker to narrow which agents' connections count toward the comparison. |
| **Raw** | Every measurement attributed to the campaign. |
| **Settings** | Knobs used by the last evaluation, plus the Re-evaluate button. |

### Heatmap color editor

In the **Heatmap** tab, click the color-tier button to open the
**HeatmapColorEditor** popover. Four boundary handles divide the
color spectrum into five tiers (excellent / good / fair / marginal /
poor). Default boundaries are computed from `useful_latency_ms`
(`0.4·T`, `T`, `2·T`, `4·T`). Drag a handle or type a value to
customize; the changes persist to localStorage at
`meshmon.evaluation.heatmap.edge_candidate.colors` so the view is
preserved across sessions.

### Compare tab agent picker

In the **Compare** tab, use the agent picker to select a subset of
source agents. The candidate stats are re-aggregated in real time
against only the selected agents, so you can see how a candidate
performs for a specific region or cluster without re-running the
campaign.

### Legacy evaluations

Evaluations made before `useful_latency_ms`, `max_hops`, and
`vm_lookback_minutes` were added carry a **Legacy** badge in the
Settings tab. Re-running **Re-evaluate** with the new knobs set will
produce a fresh evaluation row without the legacy marker.

## Evaluation guardrails

Four optional knobs gate the evaluator's output. All default to "off"
(NULL); enabling any one is purely additive over the default
behaviour.

### Eligibility caps

`max_transit_rtt_ms` and `max_transit_stddev_ms` express absolute caps
on the *composed* transit A→X→B. A pair where the composed RTT or
stddev exceeds the cap is excluded from the candidate's scoring
entirely — it doesn't bias `composite_score`, doesn't count in
`pairs_total_considered`, and doesn't appear in the per-pair drilldown.

Use eligibility caps to express "I don't care about routes slower
than X ms (or jitterier than X ms) regardless of how they compare to
the direct path."

### Storage floors

`min_improvement_ms` and `min_improvement_ratio` express minimum
improvements (signed — negatives allowed) for a per-pair scoring row
to be persisted. The candidate is still scored against the pair —
`pairs_improved` and `pairs_total_considered` reflect the full set —
but rows below either threshold aren't written to storage.

The two floors combine with **OR** semantics: a row is stored if
*either* threshold passes. "X ms or Y % better" — pick whichever
matches the operator framing.

### Tightening vs loosening

Tightening a knob between evaluations drops more rows from the new
pass. Loosening recovers them: the underlying inputs (active probe
measurements + VM-sourced direct baselines) are durable, so a re-
evaluate with looser knobs re-computes from the same inputs and
surfaces the previously-dropped data.

The drilldown's runtime filters can only tighten beyond the active
guardrails — they can't recover rows that the guardrails already
dropped from storage. Each filter input shows the active guardrail
value as placeholder text so the floor is visible.

## Detail measurements

Detail measurements re-run one or more pairs with one MTR trace plus
a 250-probe latency burst, so an operator can chase down a promising
candidate or an ambiguous result without re-running the whole
campaign. The overflow menu on the campaign page exposes three scopes:

| Scope | Selection | Approximate enqueue |
|---|---|---|
| **Detail: all pairs** | every `campaign`-kind pair in a settled state (`succeeded` / `reused`); `pending` / `dispatched` / `skipped` / `unreachable` are excluded | `2 × (settled pair count)` |
| **Detail: good candidates only** | every qualifying `(A, X, B)` triple from the latest evaluation | `4 × (qualifying-triple count)`, de-duplicated |
| **Detail this pair** (row action) | one explicit `(source, destination)` | `2` |

**Detail: good candidates only** is gated strictly on
`state === "evaluated"`. A stale evaluation on a `completed` campaign
does not unlock the action — press **Evaluate** first.

### Cost preview

Each scope opens a confirmation dialog with the scope label, the
affected pair count, and the expected `pairs_enqueued` total. Cancel
backs out without touching the backend; confirm fires the dispatch and
surfaces a toast on success or failure
(`no_pairs_selected`, `no_evaluation`, `illegal_state_transition`
funnel to operator-friendly messages rather than raw codes).

Detail dispatch flips the campaign to `running` while the sweep is
in-flight; it returns to `completed` (or `evaluated`, if a prior
evaluation exists) once every detail pair settles. Detail rows never
feed the next evaluation — they refine the operator's view of a pair
without moving the baseline. Press **Evaluate** again to fold fresh
measurements into the candidate scoring.

## Running Evaluate

Evaluate is manual. Finish a campaign, press **Evaluate**, and the
scoring runs against the campaign's attributed measurements plus —
when `[upstream] vm_url` is configured — any agent→agent pair the
campaign didn't cover itself, pulled from VictoriaMetrics
continuous-mesh data at evaluate time.

Each call appends a fresh row to `campaign_evaluations` (with
per-candidate and per-pair rows in the child tables); older rows
stay immutable. `GET /api/campaigns/{id}/evaluation` always returns
the latest.

## History

`/history/pair` renders latency, loss, and MTR traces for any
`(source, destination)` pair with at least one measurement, independent
of campaigns. Use it to answer "how has this route behaved over the
past week?" without having to find the right campaign first.

### Picker flow

Two popover pickers anchor the top of the page: **Source** (every agent
that has produced at least one measurement) and **Destination** (every
IP that source has reached). Both are filterable; destinations whose
catalogue row has been deleted render as `"<ip> — no metadata"`. The
page URL is the source of truth — picker clicks update
`?source=…&destination=…` in place, so shared links round-trip exactly.

### Time range

The range selector offers `24h`, `7d`, `30d`, `90d`, and **custom** (a
date-time pair). The latency chart overlays one line per protocol
present in the result set, plus a translucent min/max band; the loss
panel below shares the same X axis. Hovering a data point pins the
tooltip with per-protocol averages and loss.

The service caps `/api/history/measurements` at 5 000 rows. When a
query hits the cap, the page shows a "showing most recent 5 000"
notice so the operator knows to narrow the window.

### MTR traces

Below the chart, every MTR-kind measurement in the window shows as a
collapsible row. Expanding a row renders the trace in
`RouteTopology`; rows are ordered newest-first so the current
behaviour is at the top.

## Composer workflow

The sections above describe the HTTP contract. The composer UI is the
operator's front door onto that contract — the mapping from button
clicks to payloads is below.

### Start a new campaign

Navigate to `/campaigns/new`. Fill the title, pick sources (agents),
pick destinations (catalogue IPs), tune the knobs, and click **Start**.
The page keeps a local draft until **Start** is clicked — the URL does
not persist draft state, so a browser refresh loses the draft.

### Sources

Agents with `catalogue_coordinates: null` are excluded from the map
view but remain in the list. Offline agents (last heartbeat older than
5 min) carry a badge; the backend still accepts them and silently skips
their pairs after 3 dispatch attempts.

### Destinations

The destination panel uses the same **FilterRail** surface as the
catalogue (country, ASN, network, city, shapes). An inline paste flow
accepts newline-separated IPs; duplicates are reconciled against the
catalogue silently.

**Add all** walks every catalogue page that matches the current filter
(or the whole catalogue when no filter is active) and merges every IP
into the selection. An inline strip reports progress while the walk
runs; the button disables until the initial catalogue fetch lands so
the walk never races a pre-first-page snapshot. Prior manual picks and
earlier filtered walks survive the merge — operators can layer passes
to build a selection without losing previous choices. **Remove all**
is the only action that clears the set.

### Knobs

Per-campaign parameters exposed in the composer:

- `protocol` — `icmp` / `tcp` / `udp`. MTR is UI-only and blocks
  **Start**; run MTR via the per-pair **Detail** action in the results
  view instead.
- `probe_count` — probes per dispatched measurement (default 10).
- `probe_count_detail` — probes per detail re-run (default 250).
- `timeout_ms` — per-probe timeout (default 2000).
- `probe_stagger_ms` — inter-probe stagger (default 100).
- `loss_threshold_ratio` — evaluator's loss-rate threshold as a fraction
  (default 0.02, i.e. 2 %).
- `stddev_weight` — weight applied to RTT stddev (default 1.0).
- `evaluation_mode` — `diversity`, `optimization`, or `edge_candidate`
  (default `optimization`).
- `useful_latency_ms` — EdgeCandidate only. RTT threshold T (ms) below
  which a connection is "useful". Required when `evaluation_mode` is
  `edge_candidate`.
- `max_hops` — EdgeCandidate only. Maximum intermediary hops per route
  (0–2, default 2).
- `vm_lookback_minutes` — VictoriaMetrics baseline lookback window in
  minutes (default 15). Applies to all modes.

### Diversity vs Optimization

`diversity` spreads probes across as many distinct paths as possible;
`optimization` prioritises probes most likely to catch regressions. The
copy surfaced next to the toggle in `KnobPanel` is the authoritative
wording — update this doc and the component together.

### MTR

Not a campaign protocol. Operators run MTR via the per-pair **Detail**
action in the results view.

### Force measurement

When the toggle is on, the scheduler ignores the 24 h reuse cache; the
"reusable" count collapses to zero and every pair is measured fresh.

### Size preview

Before **Start**, the page shows `~{sources × destinations}` as an
approximate count (the `~` disappears once the destination set is
committed via **Add all**). After **Create**, the backend returns an
exact `total / reusable / fresh` triple. When `fresh` exceeds
`[campaigns] size_warning_threshold` in
`crates/service/src/config.rs` (default 1000), a confirmation dialog
gates **Start**.

### Stop

A running campaign can be stopped mid-run — pending pairs flip to
`skipped` server-side and the writer finishes draining any in-flight
dispatches. From terminal states operators can update metadata, clone
the campaign (next section), or delete it outright.

## Restart and Clone

Two actions take a finished campaign somewhere new. Both are offered
on the action bar of any terminal campaign (`completed`, `stopped`, or
`evaluated`).

| Action | What it does |
|---|---|
| **Restart** | Re-runs the exact same pair set against the same knobs. Existing terminal pairs carry over; only pairs that still need work dispatch. Toggle the sticky `force_measurement` flag first when a full re-measurement is wanted. |
| **Clone** | Seeds a fresh draft at `/campaigns/new` with this campaign's sources, destinations, and knobs. Edit anything before pressing Start — the clone has no effect on the original campaign. |

Clone is the tool for "run this again with tweaks". The composer
mounts pre-populated and the source / destination pickers and the
knob panel are available for edits. The clone's title defaults to
`"Copy of <original>"`; `force_measurement` resets to `false` so a
tweak-and-rerun does not silently re-probe every pair. The backend
caps the pair walk at 5 000 pairs — if the source campaign is larger,
Clone surfaces a warning toast and the operator reviews the seed before
launching.

## See also

- [Architecture](architecture.md) — data model, scheduler, 24 h reuse,
  HTTP surface.
- [Runbook](../runbook.md) — operational response.

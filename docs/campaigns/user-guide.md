# Campaigns — User Guide

Operator guide for the campaigns feature. See [`architecture.md`](architecture.md) for the developer reference.

## What campaigns are for

meshmon has a fleet of agents continuously probing each other. Campaigns let you borrow that fleet to answer a different question: *would a new server improve the mesh?*

You collect the IPs you want to test, run latency / loss measurements from the agent fleet to each one, and the results show which candidates would provide a faster or more reliable route than what the mesh has today.

Typical uses:

- **Optimisation** — find candidates that would improve specific slow routes.
- **Diversity** — find candidates that provide independent alternatives to existing routes, regardless of whether those alternatives are faster.
- **Investigation** — take a suspicious pair and drill into its MTR / loss history.

Campaigns are one-shot. They never run on a schedule and they don't replace continuous probing between mesh members.

## The IP catalogue

The **Catalogue** page holds every IP meshmon knows about — the ones you've added manually and the ones auto-created for each meshmon agent. Every entry carries an IP, a display name, a structured location (city + country + coordinates), an ASN, a network operator, an optional website, and free-text notes.

### Adding IPs

Click **Add IPs** on the catalogue page. A paste box opens that accepts one IP per line or a comma-separated batch — hundreds at a time. IPs already in the catalogue are reused (no duplicates); new IPs are saved immediately and enriched in the background. You'll see each row's fields fill in live as the enrichment providers respond.

Only bare IPv4 / IPv6 addresses are accepted. CIDR ranges are rejected with an inline error to prevent accidentally creating hundreds of entries from a single paste.

### Editing entries

Click a row to open the edit drawer. Every field is editable. Anything you change is treated as authoritative — subsequent re-enrichments won't overwrite it. Each edited field gets a "Revert to auto" link that clears your value and lets the providers populate it again on the next re-enrich.

### Re-enrichment

The **Re-enrich** button on each row asks the providers again. Use it if you think the data is stale or wrong. Your manually-edited fields stay put.

ipgeolocation's free tier has a daily quota shared across the whole deployment. One re-enrich click uses one credit. Bulk re-enrich shows a confirmation with the expected credit cost before dispatching.

### Filters

A filter rail on the catalogue page supports **City**, **Country**, **ASN**, **Network operator**, **IP**, and **Name**. Each filter is a searchable dropdown with a preview of the top unique values in your catalogue. Partial matches work — typing `oneprovider` finds everything whose name, city, etc. contains that substring.

### Map filter

Switch to the map view and click **Draw on map**. Pick a rectangle, circle, or freeform polygon; every entry whose coordinates fall inside is selected. Multiple shapes combine with OR. Useful when you want "every IP within 500 km of Frankfurt" as a starting point.

Notes are intentionally not filterable — they're for your reference only.

### Agents in the catalogue

Every meshmon agent gets a catalogue entry automatically when it registers. Agent-reported coordinates win over any provider data; the providers still fill in ASN, city, and network operator. You can still rename or edit an agent's catalogue entry — your edits stick.

## Creating a campaign

The **Campaigns** page has a **New campaign** button. A campaign needs four things:

1. A **title** and optional **notes** (both are searchable later).
2. A **protocol** — ICMP, TCP, or UDP.
3. A set of **sources** — the meshmon agents that will run the probes.
4. A set of **destinations** — the IPs they will probe.

Both source and destination selectors share the same filter panel as the catalogue (city, country, ASN, network, IP, name, plus the map). You can click individual rows to add them, use **Add all matching filter** to mass-add everything the current filter matches, or **Add all** to include every agent or every catalogue entry.

The composer shows a live count: *"Expected: 312 measurements (48 reusable from last 24 h, 264 new)."* Any pair meshmon already measured in the last 24 hours is served from cache — you don't pay to re-probe it unless you want to.

### Campaign settings

The composer exposes every knob as a per-campaign setting:

- **Probes per measurement** (default 10) — how many probes hit each pair.
- **Probes per detail measurement** (default 250) — used when you later run a detail measurement on a pair.
- **Per-probe timeout** (default 2 s).
- **Inter-probe stagger** (default 100 ms) — how quickly probes inside one measurement are fired.
- **Packet-loss threshold** (default 2 %) — routes above this don't qualify as viable candidates during evaluation.
- **Stddev weight** (default 1.0) — how much unstable latency is penalised (0 ignores instability; higher punishes it).
- **Evaluation mode** — `diversity` or `optimization` (see below).
- **Force measurement** (off by default) — skip the 24 h cache and probe every pair fresh.

### Starting

Click **Start campaign**. If the campaign will dispatch more than the configured size threshold (1,000 new measurements by default), a confirm dialog appears. Otherwise dispatch begins immediately.

### What runs, and when

- The service round-robins across active campaigns at batch granularity, so no single campaign monopolises the fleet.
- Per-agent concurrency, per-destination ingress (2 measurements/s per destination IP across the whole cluster), and batch size (up to 50 pairs per dispatch) are all bounded.
- Results stream back and land on the campaign's page live.
- If an agent goes offline or a destination is unreachable, the affected pairs are marked `skipped` or `unreachable` — the campaign still completes normally. Partial success is expected.

### Stopping

Click **Stop campaign**. Subsequent batches are cancelled; in-flight batches drain within a few seconds. All results that land during drain are kept.

### Editing after finish

A finished or evaluated campaign has an **Edit** action. Add or remove pairs and the campaign re-runs only the delta — unchanged pairs keep their results (the 24 h cache handles everything inside the window). Toggle **Force measurement** in the edit dialog if you want every pair measured fresh.

## Reading the results

Click any campaign to open its detail page. Four tabs:

### Candidates

One row per IP that could serve as a transit point. Columns: rank, name / IP, city, ASN, network, **pairs improved** (count / total baseline pairs), **avg Δ** (mean latency improvement in ms — negative is better), **loss** (colour-coded against your threshold), and a composite **score** bar.

Click a row to expand a pair-by-pair breakdown — direct latency, transit latency, improvement, link to the MTR trace if present.

Agents appearing as candidates carry a "mesh member — no acquisition needed" badge.

### Pairs

The same data pivoted: one row per baseline A→B route, showing the best candidate that improves it. Useful when your question is "where are our weakest routes and what would fix them?"

### Raw measurements

Every measurement in the campaign, filterable by resolution state (`succeeded`, `reused`, `unreachable`, `skipped`), protocol, and kind (campaign / detail_ping / detail_mtr). Good for sanity-checking what actually happened. Pairs can be force-remeasured from here individually.

### Evaluation settings

Change `loss_threshold_pct`, `stddev_weight`, or `evaluation_mode` and re-evaluate. No measurements are re-run — this is pure post-processing. The evaluation result is rewritten with the new settings.

## Diversity vs Optimization

The evaluation mode controls which candidates the evaluator considers "good".

- **Optimization** (default) — a candidate X qualifies only if `A → X → B` beats both the direct `A → B` and every alternative transit `A → Y → B` via existing mesh agents Y that the campaign measured. Use this to find candidates that would *genuinely* improve the mesh — existing agents are your real competition.
- **Diversity** — a candidate qualifies as long as `A → X → B` beats the direct route. Existing agents aren't considered as alternatives. Use this when you want to see every option — for redundancy, geographic spread, or an independent backup route — regardless of what the mesh already offers.

You can switch modes on the Evaluation settings tab and re-evaluate without re-running any measurements.

## Detail measurements

A detail measurement is a richer look at a specific pair: one MTR trace plus a 250-probe latency run in the campaign's protocol. Use it to confirm a candidate looks stable before acting on it.

Three trigger scopes from the overflow menu on the Candidates and Pairs tabs:

- **Detail: all pairs** — expensive, runs for every pair in the campaign.
- **Detail: good candidates only** — recommended; runs only for qualifying candidates.
- **Detail this pair** — a row action for one specific pair.

Each scope shows a cost preview before dispatching. Detail measurements always persist and the 24 h reuse cache prefers them over regular measurements — once you've detailed a pair, future campaigns on the same pair automatically reuse the richer data.

## Historic pair view

The **History** page lets you pick any (source agent, destination IP) that has at least one measurement in the database. You get:

- **Latency over time** — one line per protocol, min/max band, markers on points with loss.
- **Loss over time** — bars coloured by protocol.
- **MTR traces** — a timeline of every MTR for this pair; click one to open it in the hop viewer.

Time range selector: 24 h / 7 d / 30 d / 90 d / custom. This is the place to look when you want to see how a route has evolved — before and after a candidate was added, or across suspected incidents.

## Troubleshooting

**Campaign is stuck in Running.** A source agent is probably offline. Open the Raw measurements tab and filter by state = `pending`. If many rows share the same source, check that agent's last-seen time. The campaign will auto-mark the stuck pairs as `skipped` after three attempts; it doesn't stay Running forever.

**Enrichment failed on an entry.** ipgeolocation may be over quota, or the IP may be in a reserved range. Click **Re-enrich** to retry. If the failure persists, fill the fields manually — your edits will stick.

**"No baseline routes" on evaluation.** The campaign has no pair where both endpoints are meshmon agents. Evaluation needs at least one agent-agent pair to establish a baseline. Edit the campaign, add a meshmon agent as a destination, and re-run.

**Detail measurement cost looks too high.** Tighten the scope (Good candidates only instead of All pairs), or lower `probe_count_detail` on the Evaluation settings tab before dispatching.

## See also

- [Architecture](architecture.md) — developer reference covering the data model, RPCs, scheduler, and probe internals.

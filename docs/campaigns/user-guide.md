# Campaigns — User Guide

Operator guide for the catalogue surface that backs the campaigns
feature. See [`architecture.md`](architecture.md) for the developer
reference.

The campaign composer, results browser, and evaluation workflow arrive
in a later subsystem — this guide covers the catalogue.

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

## Editing

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

## Campaigns, history, and evaluation

Composing a campaign, reading results, triggering detail
measurements, and switching evaluation modes are covered by a later
subsystem. This guide is scoped to the catalogue.

## See also

- [Architecture](architecture.md) — catalogue schema, enrichment
  chain, runner, SSE broker.
- [Runbook](../runbook.md) — operational response.

# `enrichment`

Pluggable provider chain and background runner that populate
`ip_catalogue` rows with geography, ASN, and network-operator data.

## Files

| File | Role |
|---|---|
| `mod.rs` | `EnrichmentProvider` trait, `EnrichmentError`, `EnrichmentResult`, `MergedFields` (first-writer-wins + lock-skip merge). |
| `providers/mod.rs` | `build_chain(cfg)` composes the ordered provider list from config. |
| `providers/ipgeolocation.rs` | Paid geolocation provider with the widest field coverage. |
| `providers/rdap.rs` | RDAP registry lookup via `icann-rdap-client`. |
| `providers/maxmind.rs` | `enrichment-maxmind` feature — local GeoLite2 mmdb lookups. |
| `providers/whois.rs` | `enrichment-whois` feature — legacy netname fallback. |
| `runner.rs` | `EnrichmentQueue` (bounded mpsc) and `Runner` (drains the queue and sweeps stale `pending` rows). |

## Adding a provider

1. Create a new file under `providers/`.
2. Implement the `EnrichmentProvider` trait in `mod.rs`:
   - `id()` — stable, short string that appears in logs and metric
     labels. Must not change across releases.
   - `supported()` — the set of `Field`s the provider may populate.
     Return only what the provider actually fills; the merge layer
     short-circuits on already-populated fields, so advertising extra
     fields is a documentation lie rather than a bug.
   - `lookup(ip)` — async per-IP call returning `EnrichmentResult` or
     a typed `EnrichmentError`.
3. Register the provider in `providers::build_chain`, matching the
   config gating pattern used by existing entries (`enabled` flag,
   optional `cfg(feature = "…")` for optional build-time deps).
4. Chain position determines first-writer-wins priority. Earlier
   providers win conflicts; pick the slot that matches the provider's
   strength (paid / highest-coverage sources first, free / fallback
   sources last).

Providers are pure: compute fields and return them. The runner is the
single persistence point, so the operator lock contract is enforced
in one place.

## Failure model

`EnrichmentError` variants drive runner behaviour:

| Variant | Runner reaction |
|---|---|
| `RateLimited { retry_after }` | Log and move on. Row stays `pending`; the 30-second sweep re-picks it. |
| `Unauthorized` | Log at warn. The runner does not currently disable the provider — it will keep 401-ing until the credential is rotated, generating a warn log per attempt. A future patch may add per-process disable-on-401. |
| `NotFound` | Terminal for this provider; runner falls through to the next one in the chain. |
| `Transient(String)` | Log; continue the chain. If no provider wrote anything, the sweep re-picks the row. |
| `Permanent(String)` | Log; continue the chain. Not retryable for this row. |

If every provider errored and `MergedFields::any_populated()` stays
false, the repo writes terminal `enrichment_status = 'failed'` for the
row. Otherwise the row flips to `enriched`.

## Feature flags

| Feature | Default | Compiles |
|---|---|---|
| (default build) | — | `ipgeolocation` + `rdap` (both off by default; flip the `enabled` toggle per provider) |
| `enrichment-maxmind` | off | adds `maxmind` |
| `enrichment-whois` | off | adds `whois` |

Enabling a feature flag does not auto-enable the provider — the
corresponding `[enrichment.<provider>] enabled = true` setting is the
runtime gate. Conversely, setting `enabled = true` in config without
the feature compiled in is a boot-time error.

`[enrichment.ipgeolocation] enabled = true` additionally requires
`acknowledged_tos = true` — the loader aborts boot on violation.

### RDAP default

`[enrichment.rdap] enabled` defaults to `false` because the provider's
`lookup()` is currently a TODO stub that returns a permanent error for
every IP. Toggle to `true` only after the real `rdap_bootstrapped_request`
wire-up ships (tracked separately), otherwise every row that reaches the
RDAP slot walks through a guaranteed-fail path and spends log lines
without producing fields.

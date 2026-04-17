# meshmon Grafana dashboards

JSON-as-code dashboards that the meshmon frontend iframes and that operators
browse directly. This directory ships three dashboards plus a datasource
provisioning template and a contract-drift guard.

## Dashboards

| File | UID | Purpose |
|------|-----|---------|
| `dashboards/meshmon-path.json` | `meshmon-path` | Per-path RTT / loss / jitter for a `(source, target, protocol)` tuple. Iframed by the Path Detail and Report pages. Panel IDs frozen: rtt=1, loss=2, stddev=3. |
| `dashboards/meshmon-overview.json` | `meshmon-overview` | Fleet-wide health heatmap + degraded-paths stat + recent route-change table. Operator-facing. |
| `dashboards/meshmon-agent.json` | `meshmon-agent` | Per-agent outgoing + incoming path grids. Parameterized by `$source`. Operator-facing. |

Dashboard UIDs are stable — the frontend iframes reference them; renames
break embedded URLs. To extend a dashboard, add panels with new IDs (number
from 100 up) and never renumber existing panels.

## Contract

`panels.json` is the frontend/dashboard handshake for iframed panels. The
frontend imports it via the `@grafana` Vite alias and builds
`d-solo/<uid>?panelId=<id>&var-<name>=…` URLs. If a dashboard renumbers a
panel listed in `panels.json`, iframes silently fall back to the "Dashboard
not configured" placeholder.

`verify-panels.mjs` enforces the contract on every PR:

```bash
node grafana/verify-panels.mjs
```

The script checks that every `panels.json` entry's dashboard exists, declares
the matching `uid`, contains the expected panel IDs, and declares the
required template variables. It does NOT enforce the reverse direction —
dashboards not listed in `panels.json` (overview, agent) can evolve freely.

## Datasources

`provisioning/datasources.yml.template` is the **single source of truth**.
It defines two datasources:

- `MeshmonVM` — VictoriaMetrics (Prometheus-compatible). UID `MeshmonVM`.
- `MeshmonPostgres` — Postgres + TimescaleDB. UID `MeshmonPostgres`.

Operators fill in `${MESHMON_VM_URL}`, `${MESHMON_PG_URL}`, `${MESHMON_PG_USER}`,
`${MESHMON_PG_PASSWORD}`, `${MESHMON_PG_DATABASE}` via `envsubst` or their
secret tool of choice, then drop the result into Grafana's provisioning
directory. The smoke harness uses the same template — no parallel hand-
edited copy.

UIDs are pinned because every dashboard panel references them by UID.

## Grafana prerequisites

The frontend iframes panels via `/d-solo/<uid>?…&kiosk`. Two Grafana settings
are required:

1. `[security] allow_embedding = true` in `grafana.ini` (or
   `GF_SECURITY_ALLOW_EMBEDDING=true` env var) — lets the SPA embed panels
   across origins.
2. Anonymous or cookie-shared viewer access for the meshmon SPA's origin.
   Typical options: Grafana Anonymous Auth (read-only viewer role),
   OAuth-via-reverse-proxy, or a shared cookie if the two services are
   same-origin.

The bundled Grafana in `deploy/docker-compose.yml` (T24) sets both; operators
using an external Grafana configure them once per environment.

Meshmon iframes pass `theme=light` on the Report page so printed PDFs stay
legible. Grafana honours that URL parameter regardless of the dashboard's
own `style` field — no per-dashboard config needed.

## Validation

### Hermetic (CI, no containers)

```bash
./scripts/validate-dashboards.sh
```

JSON syntax + `verify-panels.mjs` contract. Runs in GitHub Actions on every
PR.

### Smoke (end-to-end, local only)

```bash
./scripts/smoke-dashboards.sh
```

Spins up `grafana/test-harness/docker-compose.yml` (VM + Grafana), seeds VM
with a synthetic `meshmon_path_*` series via `envsubst`-generated
datasources, then GETs `/d-solo/<uid>?panelId=…` for each dashboard and
asserts HTTP 200. Requires Docker + Compose v2 + `envsubst` (from gettext).
Takes ~30 s. Not run in CI.

## Editing workflow

When adding or modifying a dashboard:

1. Edit the JSON. Keep it formatted (2-space indent, trailing newline).
2. If the change touches panels listed in `panels.json`, update
   `panels.json` in the same commit. Otherwise leave `panels.json` alone.
3. `./scripts/validate-dashboards.sh` — must exit 0.
4. `./scripts/smoke-dashboards.sh` — manual, at least once per PR.
5. Open PR. CI runs the validator automatically.

Commit messages follow `feat(grafana): …` or `fix(grafana): …`.

## FAQ

**Q: Why replace the standard Grafana `alertlist` panel with a VM-native
stat on the overview?**
A: `alertlist` needs a Grafana unified-alerting or Alertmanager datasource.
meshmon routes through vmalert + Alertmanager, and the provisioning template
doesn't include an Alertmanager datasource. vmalert's `-remoteWrite.url`
flag (which would put `ALERTS` into VM) isn't configured in the default
stack either. The "Degraded paths (5 min)" stat uses path-level metrics
that are always in VM, matches the `PathPacketLoss` alert threshold exactly,
and renders "0" when the fleet is healthy. For full alert browsing, use
the meshmon `/alerts` page — it's backed by the service's Alertmanager
proxy.

**Q: Can I add a service-metrics dashboard (API latency, ingestion stats)?**
A: Not yet. The service exposes self-metrics at `/metrics` for external
Prometheus scrape, but meshmon's VM instance doesn't currently scrape them.
A `meshmon-service` dashboard is blocked on wiring `vmagent` or a VM scrape
config to hit `meshmon-service:8080/metrics`. Tracked separately.

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

The contract is enforced on every PR by a hermetic Rust test:

```bash
cargo test -p meshmon-service --test grafana_contract
```

The test checks that every `panels.json` entry's dashboard exists, declares
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
directory. The bundled compose stack consumes this same template — no
parallel hand-edited copy.

UIDs are pinned because every dashboard panel references them by UID.

## Grafana auth posture

The frontend iframes panels via `/d-solo/<uid>?…&kiosk`. Two non-negotiable
constraints govern how Grafana must be deployed:

1. **No anonymous access**, in any environment. The dashboards expose the
   full agent topology, IP addresses, and probe history; whoever can reach
   Grafana can read it all and pivot through Explore against the
   `MeshmonVM` / `MeshmonPostgres` datasources directly.
2. **No second login.** The user authenticates against meshmon once;
   iframes must inherit that session. Asking the user to log into Grafana
   separately is not an acceptable UX.

The sanctioned architecture is **meshmon-service as a session-authenticated
reverse proxy in front of an internal Grafana**, with Grafana running in
`auth.proxy` mode:

- Grafana binds to localhost / a docker bridge — never reachable from the
  operator network directly.
- Meshmon-service exposes `/grafana/*`, gated by the same tower-sessions
  middleware as every other endpoint, and forwards requests upstream with
  an injected `X-WEBAUTH-USER` header naming the session's username.
- Grafana `[auth.proxy]` is enabled with `header_name = X-WEBAUTH-USER`,
  `whitelist = 127.0.0.1` (so only the meshmon process can speak that
  header), and `auto_sign_up = true` so user records appear on first
  request without a manual provisioning step.
- Same-origin from the browser's POV (`grafana_base_url = "/grafana"`).
  CSP `frame-src 'self'` is sufficient; `allow_embedding = true` strictly
  speaking not required.

This mirrors the established Alertmanager-proxy pattern in
`crates/service/src/http/alerts_proxy.rs`: meshmon plays "authenticated
edge to internal infra," and the upstream's own auth posture stays
trivial because nothing else can reach it.

The meshmon-service proxy is mounted at `/grafana/{*tail}` in
`crates/service/src/http/grafana_proxy.rs`. Operators wire it via the
`[upstream] grafana_url` field in `meshmon.toml`. The bundled compose
sets it to `http://meshmon-grafana:3000/grafana` (note: Grafana is
configured with `serve_from_sub_path = true`, so the upstream URL
re-adds the `/grafana` suffix).

The bundled Grafana ships with the `auth.proxy` configuration below.
Copy it into any custom Grafana config when deviating from the bundled
compose:

```ini
[server]
serve_from_sub_path = true
root_url = %(protocol)s://%(domain)s/grafana/

[auth]
disable_login_form = true
disable_signout_menu = true

[auth.anonymous]
enabled = false

[auth.proxy]
enabled = true
header_name = X-WEBAUTH-USER
header_property = username
auto_sign_up = true
sync_ttl = 60
# The only legitimate caller is meshmon-service on the docker bridge.
# Widen `whitelist` only if the deployment topology requires it.
whitelist = 127.0.0.1, ::1, <docker bridge CIDR>
enable_login_token = false

[security]
allow_embedding = true
cookie_samesite = lax
```

The `MeshmonPostgres` datasource uses the read-only `meshmon_grafana`
role, not the service's full-privilege user. Set
`MESHMON_PG_GRAFANA_PASSWORD` at deploy time; the service's migration
bootstrap flips the role from NOLOGIN to LOGIN atomically.

### Restricted-privilege Postgres (AWS RDS, Cloud SQL, Supabase, …)

The bootstrap migration creates `meshmon_grafana` via `CREATE ROLE`,
which needs the cluster-level `CREATEROLE` privilege. When meshmon
runs against a managed Postgres instance that grants only
database-scoped privileges, the migration falls back to a
`RAISE WARNING` and the service boots without the role. The bundled
Grafana datasource won't work until an operator with `CREATEROLE`
provisions the role out-of-band:

```sql
DO $$
BEGIN
    CREATE ROLE meshmon_grafana NOLOGIN;
EXCEPTION
    WHEN duplicate_object OR unique_violation THEN NULL;
END$$;

REVOKE ALL ON ALL TABLES IN SCHEMA public FROM meshmon_grafana;
REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM meshmon_grafana;
GRANT USAGE ON SCHEMA public TO meshmon_grafana;
GRANT SELECT ON agents TO meshmon_grafana;
GRANT SELECT ON route_snapshots TO meshmon_grafana;
ALTER DEFAULT PRIVILEGES IN SCHEMA public
  REVOKE ALL ON TABLES FROM meshmon_grafana;
```

(Re-running the block is safe: the DO wrapper swallows the
`duplicate_object` if the role already exists, and the REVOKEs +
GRANTs are idempotent.)

Once the role exists, the service's startup `apply_grafana_role_password`
takes over: it flips NOLOGIN → LOGIN + sets the password whenever
`MESHMON_PG_GRAFANA_PASSWORD` is set.

Meshmon iframes pass `theme=light` on the Report page so printed PDFs stay
legible. Grafana honours that URL parameter regardless of the dashboard's
own `style` field — no per-dashboard config needed.

## Validation

**Validation:** `cargo test -p meshmon-service --test grafana_contract` (hermetic; cross-checks `panels.json` against dashboard JSONs). End-to-end dashboards-are-provisioned smoke: `cargo e2e` (requires the bundled compose stack to be running).

## Editing workflow

When adding or modifying a dashboard:

1. Edit the JSON. Keep it formatted (2-space indent, trailing newline).
2. If the change touches panels listed in `panels.json`, update
   `panels.json` in the same commit. Otherwise leave `panels.json` alone.
3. `cargo test -p meshmon-service --test grafana_contract` — must exit 0.
4. `cargo e2e` — manual, at least once per PR (requires the bundled compose stack to be running).
5. Open PR. CI runs the contract test automatically.

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

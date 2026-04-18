# Deployment

meshmon ships a bundled docker-compose that brings up the full stack:
service, Postgres (TimescaleDB), VictoriaMetrics, vmalert, Alertmanager,
and a bundled Grafana sitting behind the meshmon-service authenticated
reverse proxy.

> ⚠️ **DO NOT expose meshmon on the public internet without TLS.** The
> default compose serves plain HTTP on `:8080`. Agent tokens, session
> cookies, and login passwords all travel in cleartext. Terminate TLS
> either in-process (§ Enabling TLS) or at an external reverse proxy
> (nginx-proxy, Caddy, Traefik) before routing public traffic.

## Quick start (OSS bundled compose)

Prerequisites: Docker + Docker Compose v2. ≥ 2 GiB RAM, ≥ 20 GiB disk
headroom for VM + Postgres on a small fleet.

```bash
git clone https://github.com/w3champions/meshmon && cd meshmon

# 1. Configure secrets and config.
cp deploy/.env.example deploy/.env
$EDITOR deploy/.env            # admin password hash, agent token, PG passwords

cp deploy/meshmon.example.toml deploy/meshmon.toml

# 2. Start the stack.
cd deploy
docker compose up -d           # pulls published images
# or:
docker compose up -d --build   # builds meshmon-service + meshmon-grafana locally

# 3. (optional) Add Discord webhook URLs to deploy/.env to wire up alerts.
$EDITOR .env
docker compose up -d --force-recreate meshmon-alertmanager
```

Browse to `http://localhost:8080/` and log in as `admin` with the
plaintext password whose hash you set in `.env`. The SPA serves the
overview page; Grafana iframes render inline via the `/grafana/*`
proxy (no second login).

> ⚠️ **Escape `$` in `.env`.** Compose interpolates `$VAR` / `${VAR}` in
> `.env` values. Every literal `$` must be doubled to `$$`. Argon2 PHC
> hashes contain `$` separators, so an un-escaped hash becomes a blank
> string after interpolation. Generate with the snippet from
> `deploy/.env.example` (pipes through `sed 's/\$/$$/g'`).

## Published images

CI publishes three images to `ghcr.io/w3champions/` on every push to
`main` and on tagged releases, built for `linux/amd64` and `linux/arm64`:

- `meshmon-service` — the Axum API service with the embedded React SPA.
- `meshmon-agent` — the per-node probe agent.
- `meshmon-grafana` — bundled Grafana with dashboards, provisioning, and
  `grafana.ini` baked in.

The bundled compose references `:latest` by default. Pin to `:main-<sha>`
in `deploy/docker-compose.yml` for reproducible deploys.

### Image tag conventions

- `ghcr.io/w3champions/meshmon-service:latest` — rolling head of `main`. CI
  publishes this on every green build after a Trivy CRITICAL scan.
- `ghcr.io/w3champions/meshmon-service:main-<sha>` — per-commit immutable
  tag. Use this in production to avoid silent upgrades.
- `ghcr.io/w3champions/meshmon-service:v<major>.<minor>.<patch>` — published
  when a `v*` git tag is pushed. Same shape for `meshmon-agent` and
  `meshmon-grafana`.

Pinning example (edit `deploy/docker-compose.yml`):

```yaml
services:
  meshmon-service:
    # Replace <sha> with the full 40-character commit SHA emitted by the
    # release workflow. See the image's GHCR tag list for current values.
    image: ghcr.io/w3champions/meshmon-service:main-<sha>
```

Upgrade by editing the pin and `docker compose up -d --pull always meshmon-service`.

## Enabling TLS

TLS MUST be terminated before any public-internet traffic reaches the
service.

### Option 1 — in-process rustls (no external proxy)

1. Place cert + key on the host, readable by the meshmon-service
   container.
2. Uncomment `[agent_api.tls]` in `deploy/meshmon.toml` and point at
   the container-local paths.
3. Add a volume mount in `deploy/docker-compose.yml` under
   `meshmon-service.volumes:`.
4. `docker compose restart meshmon-service`.
5. Rotate certs in place with `docker kill -s HUP meshmon-service`.

Agents connect with `MESHMON_SERVICE_URL=https://<host>` — ALPN
auto-negotiates HTTP/2 for gRPC on the same `:8080` socket.

### Option 2 — external reverse proxy

Stand nginx / Caddy / Traefik in front; terminate TLS there. The proxy
MUST speak HTTP/2 upstream or `/meshmon.AgentApi/*` breaks (tonic
rejects HTTP/1.1). nginx: use `grpc_pass` for the gRPC location. Caddy:
default `reverse_proxy` speaks HTTP/2 natively.

Leave `[agent_api.tls]` commented out.

## Running agents

On each host you want to probe from, drop in the bundled agent compose.
Copy just the two files from `deploy/` — no repo checkout required:

```bash
curl -LO https://raw.githubusercontent.com/w3champions/meshmon/main/deploy/docker-compose.agent.yml
curl -LO https://raw.githubusercontent.com/w3champions/meshmon/main/deploy/agent.env.example
cp agent.env.example agent.env
$EDITOR agent.env                               # per-host identity + token
docker compose -f docker-compose.agent.yml --env-file agent.env up -d
```

`agent.env` carries `MESHMON_AGENT_TOKEN` (must match the central
service's `deploy/.env`), `MESHMON_SERVICE_URL`, and per-host identity
(`AGENT_ID`, `AGENT_IP`, `AGENT_LAT`, `AGENT_LON`, etc.). The example
file is self-documented.

The compose service runs with `network_mode: host` and
`cap_add: [NET_RAW, NET_ADMIN]` — required for ICMP/trippy raw sockets
and to bind the probe ports on real host interfaces without bridge
NAT rewriting peer addresses. **Run agents on Linux**; Docker
Desktop's host-network emulation on macOS/Windows is incomplete.

The agent opens an outbound gRPC stream to the service. **No inbound
ports required on agent hosts** beyond the peer-to-peer probe ports
(`MESHMON_TCP_PROBE_PORT`, default 3555/TCP; `MESHMON_UDP_PROBE_PORT`,
default 3552/UDP), which must be reachable on `AGENT_IP` from every
other agent.

For bare-metal (no Docker) deployments, or the full env-var table, see
the README `## Running the agent` section.

## Discord alert notifications

Discord webhook URLs are injected via docker-compose's native
`secrets:` stanza with `environment:` source — compose materializes
each env var as a file inside the Alertmanager container. Nothing
touches the host filesystem or the repo.

To enable:

1. Create the Discord webhook(s) in Discord channel settings.
2. Add to `deploy/.env`:

   ```bash
   MESHMON_DISCORD_WEBHOOK=https://discord.com/api/webhooks/<id>/<token>
   MESHMON_DISCORD_WEBHOOK_CRITICAL=https://discord.com/api/webhooks/<id>/<token>
   MESHMON_DISCORD_WEBHOOK_INFO=https://discord.com/api/webhooks/<id>/<token>
   ```

3. Recreate Alertmanager:

   ```bash
   docker compose up -d --force-recreate meshmon-alertmanager
   ```

Unset env vars produce empty secret files — AM logs a loud delivery
error rather than silently dropping alerts.

## vmalert vs Alertmanager

Two processes, both needed for alerts to reach humans.

- **vmalert** is the *rule evaluator*. Reads `deploy/alerts/rules.yaml`,
  queries VictoriaMetrics every 30 s, evaluates PromQL / MetricsQL
  expressions. When a rule's condition becomes true, vmalert emits an
  alert object to Alertmanager. vmalert has no routing, no grouping,
  no UI.
- **Alertmanager** is the *router and notifier*. Receives alerts from
  vmalert, applies grouping / inhibition / silencing / routing, then
  delivers to receivers (Discord in our setup).

Data flow: `agent → service → VM (store) → vmalert (evaluate) →
Alertmanager (group, route) → Discord`.

## Developer workflow

For local frontend + backend iteration (Vite HMR + `cargo run` service),
use `scripts/dev.sh`. It:

1. Brings up the infra stack via the dev overlay
   (`deploy/docker-compose.dev.yml`) — infra only, no meshmon-service.
2. Seeds a handful of agents and synthetic metrics.
3. Starts `cargo run -p meshmon-service` in the background against the
   infra.
4. Runs `npm run dev` in `frontend/` in the foreground with HMR.

Ctrl-C tears everything down. See `scripts/dev.sh` for env-var
overrides.

## Connecting an external Grafana to meshmon's metric sources

The bundled Grafana is the only Grafana the meshmon SPA iframes.
Operators who want to author additional dashboards in their own
Grafana instance consume meshmon's datasources directly:

1. Copy `grafana/provisioning/datasources.yml.template` to the
   external Grafana's provisioning directory.
2. Expand the `${MESHMON_...}` placeholders from that Grafana's env:
   - `MESHMON_VM_URL` — e.g. `http://meshmon-vm:8428` if the external
     Grafana joins the `meshmon-internal` network.
   - `MESHMON_PG_URL` — `meshmon-db:5432`.
   - `MESHMON_PG_DATABASE` — `meshmon`.
   - `MESHMON_PG_GRAFANA_PASSWORD` — same value used in `deploy/.env`.
3. Restart the external Grafana.

**Do not** host-map `meshmon-vm:8428` or `meshmon-db:5432` to expose
them. Prefer joining the `meshmon-internal` docker network, or set up
a dedicated docker network shared between the two stacks. The
internal-only posture is the entire security model.

## Upgrades and rollback

Bump tags in `deploy/docker-compose.yml` (or rely on `:latest`), then:

```bash
cd deploy
docker compose pull
docker compose up -d
```

To roll back meshmon-service, change `:latest` to a prior `:main-<sha>`
tag and `docker compose up -d`.

Postgres and VM volumes persist across `docker compose down && up -d`.
Schema migrations are forward-only — restore from `pg_dump` to roll
back a breaking migration.

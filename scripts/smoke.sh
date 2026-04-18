#!/usr/bin/env bash
# Local smoke-test harness for the meshmon service + frontend.
#
# Spins up Postgres + VictoriaMetrics containers, seeds a handful of agents
# and route snapshots, starts the service in the background, and runs the
# Vite dev server in the foreground (which proxies /api to the service
# and gives HMR on frontend edits). Ctrl-C tears everything down.
#
# For a release-mode binary that serves the SPA from its own embedded
# copy, use `scripts/build-release.sh` and run the resulting binary
# directly instead.
#
# Not for production. For the full stack (vmalert, alertmanager, grafana)
# see deploy/docker-compose.yml once T24 fills it in.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT"

DB_CONTAINER=meshmon-smoke-db
VM_CONTAINER=meshmon-smoke-vm
GRAFANA_CONTAINER=meshmon-smoke-grafana
AM_CONTAINER=meshmon-smoke-am
NETWORK=meshmon-smoke-net
DB_PORT=${MESHMON_SMOKE_DB_PORT:-5432}
VM_PORT=${MESHMON_SMOKE_VM_PORT:-8428}
# Non-standard default port to avoid clashing with Docker Desktop's common
# *:8080 binding and other typical local dev services.
SERVICE_PORT=${MESHMON_SMOKE_SERVICE_PORT:-18080}
FRONTEND_PORT=${MESHMON_SMOKE_FRONTEND_PORT:-5173}
CONFIG_PATH=${MESHMON_SMOKE_CONFIG:-/tmp/meshmon-smoke.toml}
ADMIN_USER=${MESHMON_SMOKE_USER:-admin}
ADMIN_PASSWORD=${MESHMON_SMOKE_PASSWORD:-smoketest}
SERVICE_LOG=${MESHMON_SMOKE_SERVICE_LOG:-/tmp/meshmon-smoke-service.log}

TIMESCALE_IMAGE=timescale/timescaledb:2.26.3-pg16
VM_IMAGE=victoriametrics/victoria-metrics:v1.104.0

# Image tags for bundled Grafana + Alertmanager come from the single source
# of truth in deploy/versions.env so the smoke harness stays pinned to the
# same images as CI + production.
if [[ -f deploy/versions.env ]]; then
  # shellcheck disable=SC1091
  source deploy/versions.env
fi
GRAFANA_TAG=${GRAFANA_TAG:-13.0.1}
ALERTMANAGER_TAG=${ALERTMANAGER_TAG:-v0.32.0}

# Resolve the docker-assigned ephemeral host port for a container's internal
# port. We request "127.0.0.1:" in the -p flag so docker picks a free port
# on loopback; `docker port` then reports host_ip:host_port.
port_of() {
  local container="$1" internal="$2"
  docker port "$container" "${internal}/tcp" 2>/dev/null \
    | awk -F: '/^127\.0\.0\.1:/ {print $2; exit}'
}

SERVICE_PID=
teardown() {
  echo
  echo "[smoke] tearing down"
  if [[ -n "$SERVICE_PID" ]] && kill -0 "$SERVICE_PID" 2>/dev/null; then
    kill "$SERVICE_PID" 2>/dev/null || true
    wait "$SERVICE_PID" 2>/dev/null || true
  fi
  docker rm -f "$DB_CONTAINER" >/dev/null 2>&1 || true
  docker rm -f "$VM_CONTAINER" >/dev/null 2>&1 || true
  docker rm -f "$GRAFANA_CONTAINER" >/dev/null 2>&1 || true
  docker rm -f "$AM_CONTAINER"      >/dev/null 2>&1 || true
  docker network rm "$NETWORK" >/dev/null 2>&1 || true
}
trap teardown EXIT INT TERM

require() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "[smoke] error: '$1' is required but not installed"
    exit 1
  }
}
require docker
require cargo
require argon2
require openssl
require psql
require sqlx
require npm

# ---- Docker network -----------------------------------------------------
# User-defined bridge network lets the bundled Grafana + Alertmanager resolve
# each other (and Postgres / VM) by container name, mirroring the topology
# operators ship in deploy/docker-compose.yml.
docker network rm "$NETWORK" >/dev/null 2>&1 || true
docker network create "$NETWORK" >/dev/null

# ---- Postgres -----------------------------------------------------------
docker rm -f "$DB_CONTAINER" >/dev/null 2>&1 || true
docker run --rm -d --name "$DB_CONTAINER" \
  --network "$NETWORK" \
  -e POSTGRES_USER=meshmon -e POSTGRES_PASSWORD=meshmon -e POSTGRES_DB=meshmon \
  -p "${DB_PORT}:5432" "$TIMESCALE_IMAGE" >/dev/null

echo "[smoke] waiting for Postgres on :${DB_PORT}"
until docker exec "$DB_CONTAINER" pg_isready -U meshmon >/dev/null 2>&1; do
  sleep 0.5
done

# ---- VictoriaMetrics ----------------------------------------------------
docker rm -f "$VM_CONTAINER" >/dev/null 2>&1 || true
docker run --rm -d --name "$VM_CONTAINER" \
  --network "$NETWORK" \
  -p "${VM_PORT}:8428" "$VM_IMAGE" >/dev/null

echo "[smoke] waiting for VictoriaMetrics on :${VM_PORT}"
until curl -fs "http://127.0.0.1:${VM_PORT}/health" >/dev/null 2>&1; do
  sleep 0.5
done

# ---- Alertmanager -------------------------------------------------------
# Bundled Alertmanager so the "View in Alertmanager" links on the Alerts
# page render against a real instance. The container binds 9093 to an
# ephemeral loopback port so parallel smoke runs don't collide.
#
# We mount a minimal alertmanager.yml (null receiver) because the stock
# image fails to start without one, and the smoke harness has no use for
# real routing — we only need the HTTP API reachable behind the same
# /alertmanager sub-path the service proxies to in prod.
AM_CONFIG_PATH=/tmp/meshmon-smoke-alertmanager.yml
cat > "$AM_CONFIG_PATH" <<EOF
route:
  receiver: 'null'
receivers:
  - name: 'null'
EOF

docker rm -f "$AM_CONTAINER" >/dev/null 2>&1 || true
# --web.external-url must be a full URL including the sub-path; v0.32.0
# rejects a bare path. The hostname is cosmetic (it only appears in
# generated links); the path is what configures the route prefix, so
# /alertmanager here mirrors production's sub-path routing.
docker run --rm -d --name "$AM_CONTAINER" \
  --network "$NETWORK" \
  -p "127.0.0.1::9093" \
  -v "${AM_CONFIG_PATH}:/etc/alertmanager/alertmanager.yml:ro" \
  "prom/alertmanager:${ALERTMANAGER_TAG}" \
  --config.file=/etc/alertmanager/alertmanager.yml \
  --web.external-url=http://127.0.0.1/alertmanager \
  --storage.path=/tmp/am-data \
  >/dev/null

AM_PORT=$(port_of "$AM_CONTAINER" 9093)
if [[ -z "$AM_PORT" ]]; then
  echo "[smoke] error: could not resolve Alertmanager host port" >&2
  exit 1
fi

# Sub-path routing means the ready endpoint moves from /-/ready to
# /alertmanager/-/ready — same as the production deployment.
echo "[smoke] waiting for Alertmanager on :${AM_PORT}"
until curl -fs "http://127.0.0.1:${AM_PORT}/alertmanager/-/ready" >/dev/null 2>&1; do
  sleep 0.5
done

# ---- Grafana ------------------------------------------------------------
# Bundled Grafana OSS mounted with the harness grafana.ini (auth.proxy mode,
# serve_from_sub_path). Ephemeral loopback port keeps the harness safe to
# run on hosts already bound to :3000.
docker rm -f "$GRAFANA_CONTAINER" >/dev/null 2>&1 || true
docker run --rm -d --name "$GRAFANA_CONTAINER" \
  --network "$NETWORK" \
  -p "127.0.0.1::3000" \
  -v "$(pwd)/grafana/test-harness/grafana.ini:/etc/grafana/grafana.ini:ro" \
  "grafana/grafana-oss:${GRAFANA_TAG}" \
  >/dev/null

GRAFANA_PORT=$(port_of "$GRAFANA_CONTAINER" 3000)
if [[ -z "$GRAFANA_PORT" ]]; then
  echo "[smoke] error: could not resolve Grafana host port" >&2
  exit 1
fi

# grafana.ini sets serve_from_sub_path = true, so the health endpoint moves
# from /api/health to /grafana/api/health — mirrors the production URL.
echo "[smoke] waiting for Grafana on :${GRAFANA_PORT}"
until curl -fs "http://127.0.0.1:${GRAFANA_PORT}/grafana/api/health" >/dev/null 2>&1; do
  sleep 0.5
done

# ---- Config -------------------------------------------------------------
echo "[smoke] hashing admin password"
SALT=$(openssl rand -base64 16)
PASSWORD_HASH=$(echo -n "$ADMIN_PASSWORD" | argon2 "$SALT" -id -t 2 -m 17 -p 1 -e)

echo "[smoke] writing config to $CONFIG_PATH"
cat > "$CONFIG_PATH" <<EOF
[service]
listen_addr = "127.0.0.1:${SERVICE_PORT}"
shutdown_deadline_seconds = 5
trust_forwarded_headers = false

[database]
url = "postgres://meshmon:meshmon@127.0.0.1:${DB_PORT}/meshmon?sslmode=disable"

[logging]
filter = "info,meshmon_service=debug,tower_http=info"
format = "compact"

[[auth.users]]
username = "${ADMIN_USER}"
password_hash = "${PASSWORD_HASH}"

[agent_api]
shared_token = "smoke-token-unused"

[upstream]
vm_url = "http://127.0.0.1:${VM_PORT}"
# Both bundled services serve under the same sub-path the meshmon reverse
# proxies expect (see grafana_proxy.rs / alertmanager_proxy.rs): the
# proxy strips its mount prefix off the incoming request and appends the
# remainder to the upstream URL, so the upstream base must itself include
# the sub-path. This mirrors the production topology where Grafana's
# root_url + AM's --web.external-url both sit under /grafana and
# /alertmanager respectively.
alertmanager_url = "http://127.0.0.1:${AM_PORT}/alertmanager"
grafana_url = "http://127.0.0.1:${GRAFANA_PORT}/grafana"

[agents]
target_active_window_minutes = 5
refresh_interval_seconds = 10

[probing]
udp_probe_secret = "hex:0123456789abcdef"
EOF

# ---- Migrations + seed data --------------------------------------------
echo "[smoke] running migrations"
DATABASE_URL="postgres://meshmon:meshmon@127.0.0.1:${DB_PORT}/meshmon" \
  sqlx migrate run --source crates/service/migrations >/dev/null

echo "[smoke] seeding agents + route snapshots"
# Staleness variety mirrors the three health states the agent state machine
# classifies: recent (fra-01), slightly stale (lon-01), offline (nrt-01).
# Snapshots are authored to demonstrate the T19 diff detection + T20 Report
# page: fra-01 → lon-01 has four ICMP snapshots over ~40 min where hop 2
# swaps IPs halfway through (10.1.1.2 → 10.1.1.9), so BEFORE/AFTER on the
# Report page highlights the change and the history table has rows to sort.
PGPASSWORD=meshmon psql -h 127.0.0.1 -p "$DB_PORT" -U meshmon -d meshmon -v ON_ERROR_STOP=1 >/dev/null <<'SQL'
INSERT INTO agents (id, display_name, location, ip, lat, lon, agent_version, registered_at, last_seen_at, tcp_probe_port, udp_probe_port)
VALUES
  ('fra-01', 'Frankfurt 01', 'Frankfurt, DE', '10.10.0.1', 50.11,   8.68, '0.1.0', now() - interval '1 day', now(),                         7676, 7677),
  ('lon-01', 'London 01',    'London, UK',    '10.10.0.2', 51.51,  -0.13, '0.1.0', now() - interval '1 day', now() - interval '1 minute',   7676, 7677),
  ('nrt-01', 'Tokyo 01',     'Tokyo, JP',     '10.10.0.3', 35.68, 139.69, '0.1.0', now() - interval '1 day', now() - interval '30 minutes', 7676, 7677)
ON CONFLICT (id) DO NOTHING;

INSERT INTO route_snapshots (source_id, target_id, protocol, observed_at, hops, path_summary) VALUES
  -- fra-01 -> lon-01 icmp: 4 snapshots showing a hop-2 IP swap around T-10min.
  ('fra-01', 'lon-01', 'icmp', now() - interval '40 minutes',
    '[
      {"position": 1, "avg_rtt_micros":  1200, "stddev_rtt_micros":  50, "loss_pct": 0.0, "observed_ips": [{"ip": "10.1.1.1", "freq": 1.0}]},
      {"position": 2, "avg_rtt_micros":  8500, "stddev_rtt_micros": 400, "loss_pct": 0.0, "observed_ips": [{"ip": "10.1.1.2", "freq": 1.0}]},
      {"position": 3, "avg_rtt_micros": 14200, "stddev_rtt_micros": 600, "loss_pct": 0.0, "observed_ips": [{"ip": "10.1.1.3", "freq": 1.0}]}
    ]'::jsonb,
    '{"avg_rtt_micros": 14200, "hop_count": 3, "loss_pct": 0.0}'::jsonb),
  ('fra-01', 'lon-01', 'icmp', now() - interval '20 minutes',
    '[
      {"position": 1, "avg_rtt_micros":  1100, "stddev_rtt_micros":  40, "loss_pct": 0.0,  "observed_ips": [{"ip": "10.1.1.1", "freq": 1.0}]},
      {"position": 2, "avg_rtt_micros":  8700, "stddev_rtt_micros": 350, "loss_pct": 0.0,  "observed_ips": [{"ip": "10.1.1.2", "freq": 1.0}]},
      {"position": 3, "avg_rtt_micros": 13900, "stddev_rtt_micros": 500, "loss_pct": 0.0,  "observed_ips": [{"ip": "10.1.1.3", "freq": 1.0}]}
    ]'::jsonb,
    '{"avg_rtt_micros": 13900, "hop_count": 3, "loss_pct": 0.0}'::jsonb),
  ('fra-01', 'lon-01', 'icmp', now() - interval '10 minutes',
    '[
      {"position": 1, "avg_rtt_micros":  1300, "stddev_rtt_micros":  80, "loss_pct": 0.0,  "observed_ips": [{"ip": "10.1.1.1", "freq": 1.0}]},
      {"position": 2, "avg_rtt_micros": 11200, "stddev_rtt_micros": 900, "loss_pct": 0.03, "observed_ips": [{"ip": "10.1.1.9", "freq": 1.0}]},
      {"position": 3, "avg_rtt_micros": 17800, "stddev_rtt_micros": 700, "loss_pct": 0.0,  "observed_ips": [{"ip": "10.1.1.3", "freq": 1.0}]}
    ]'::jsonb,
    '{"avg_rtt_micros": 17800, "hop_count": 3, "loss_pct": 0.01}'::jsonb),
  ('fra-01', 'lon-01', 'icmp', now() - interval '2 minutes',
    '[
      {"position": 1, "avg_rtt_micros":  1250, "stddev_rtt_micros":  60, "loss_pct": 0.0, "observed_ips": [{"ip": "10.1.1.1", "freq": 1.0}]},
      {"position": 2, "avg_rtt_micros": 10800, "stddev_rtt_micros": 750, "loss_pct": 0.0, "observed_ips": [{"ip": "10.1.1.9", "freq": 1.0}]},
      {"position": 3, "avg_rtt_micros": 17200, "stddev_rtt_micros": 650, "loss_pct": 0.0, "observed_ips": [{"ip": "10.1.1.3", "freq": 1.0}]}
    ]'::jsonb,
    '{"avg_rtt_micros": 17200, "hop_count": 3, "loss_pct": 0.0}'::jsonb),

  -- lon-01 -> nrt-01 udp: 2 snapshots, stable route, ~250 ms intercontinental.
  ('lon-01', 'nrt-01', 'udp', now() - interval '15 minutes',
    '[
      {"position": 1, "avg_rtt_micros":   1800, "stddev_rtt_micros":  100, "loss_pct": 0.0, "observed_ips": [{"ip": "10.2.2.1", "freq": 1.0}]},
      {"position": 2, "avg_rtt_micros": 120000, "stddev_rtt_micros": 4500, "loss_pct": 0.0, "observed_ips": [{"ip": "10.2.2.2", "freq": 1.0}]},
      {"position": 3, "avg_rtt_micros": 248000, "stddev_rtt_micros": 6200, "loss_pct": 0.0, "observed_ips": [{"ip": "10.2.2.3", "freq": 1.0}]}
    ]'::jsonb,
    '{"avg_rtt_micros": 248000, "hop_count": 3, "loss_pct": 0.0}'::jsonb),
  ('lon-01', 'nrt-01', 'udp', now() - interval '3 minutes',
    '[
      {"position": 1, "avg_rtt_micros":   1900, "stddev_rtt_micros":  110, "loss_pct": 0.0, "observed_ips": [{"ip": "10.2.2.1", "freq": 1.0}]},
      {"position": 2, "avg_rtt_micros": 119500, "stddev_rtt_micros": 4200, "loss_pct": 0.0, "observed_ips": [{"ip": "10.2.2.2", "freq": 1.0}]},
      {"position": 3, "avg_rtt_micros": 251000, "stddev_rtt_micros": 6400, "loss_pct": 0.0, "observed_ips": [{"ip": "10.2.2.3", "freq": 1.0}]}
    ]'::jsonb,
    '{"avg_rtt_micros": 251000, "hop_count": 3, "loss_pct": 0.0}'::jsonb),

  -- fra-01 -> nrt-01 tcp: one 5-hop snapshot, ~280 ms.
  ('fra-01', 'nrt-01', 'tcp', now() - interval '8 minutes',
    '[
      {"position": 1, "avg_rtt_micros":   1400, "stddev_rtt_micros":   70, "loss_pct": 0.0, "observed_ips": [{"ip": "10.3.3.1", "freq": 1.0}]},
      {"position": 2, "avg_rtt_micros":  14500, "stddev_rtt_micros":  600, "loss_pct": 0.0, "observed_ips": [{"ip": "10.3.3.2", "freq": 1.0}]},
      {"position": 3, "avg_rtt_micros": 142000, "stddev_rtt_micros": 5000, "loss_pct": 0.0, "observed_ips": [{"ip": "10.3.3.3", "freq": 1.0}]},
      {"position": 4, "avg_rtt_micros": 210000, "stddev_rtt_micros": 5800, "loss_pct": 0.0, "observed_ips": [{"ip": "10.3.3.4", "freq": 1.0}]},
      {"position": 5, "avg_rtt_micros": 278000, "stddev_rtt_micros": 6500, "loss_pct": 0.0, "observed_ips": [{"ip": "10.3.3.5", "freq": 1.0}]}
    ]'::jsonb,
    '{"avg_rtt_micros": 278000, "hop_count": 5, "loss_pct": 0.0}'::jsonb);
SQL

# ---- VictoriaMetrics sample seed ---------------------------------------
# Path overview reads two series from VM via MetricsQL:
#   - meshmon_path_rtt_avg_micros{source,target,protocol}   (rtt in μs → / 1000 for ms)
#   - meshmon_path_failure_rate{source,target,protocol}     (0..1 loss fraction)
# Normally the ingestion layer publishes these from agent probe batches;
# smoke has no agents, so push a synthetic story directly via VM's
# /api/v1/import (NDJSON: one line per series). Story mirrors the
# route-snapshot timeline: stable baseline, hop change at T-10min raises
# RTT and briefly spikes loss on fra-01 → lon-01, then restabilises.
echo "[smoke] seeding VictoriaMetrics sample series"
NOW_MS=$(($(date +%s) * 1000))
ts() { echo $((NOW_MS - $1 * 60 * 1000)); }

curl -fsS -XPOST "http://127.0.0.1:${VM_PORT}/api/v1/import" \
  -H "Content-Type: application/x-ndjson" \
  --data-binary @- <<NDJSON >/dev/null
{"metric":{"__name__":"meshmon_path_rtt_avg_micros","source":"fra-01","target":"lon-01","protocol":"icmp"},"values":[14000,14200,13900,14100,13800,14000,11200,17800,17500,17200],"timestamps":[$(ts 40),$(ts 35),$(ts 30),$(ts 25),$(ts 20),$(ts 15),$(ts 10),$(ts 7),$(ts 4),$(ts 1)]}
{"metric":{"__name__":"meshmon_path_failure_rate","source":"fra-01","target":"lon-01","protocol":"icmp"},"values":[0,0,0,0,0,0,0.02,0.03,0.01,0],"timestamps":[$(ts 40),$(ts 35),$(ts 30),$(ts 25),$(ts 20),$(ts 15),$(ts 10),$(ts 7),$(ts 4),$(ts 1)]}
{"metric":{"__name__":"meshmon_path_rtt_avg_micros","source":"lon-01","target":"nrt-01","protocol":"udp"},"values":[248000,249000,250000,249500,251000,250500],"timestamps":[$(ts 15),$(ts 12),$(ts 9),$(ts 6),$(ts 3),$(ts 1)]}
{"metric":{"__name__":"meshmon_path_failure_rate","source":"lon-01","target":"nrt-01","protocol":"udp"},"values":[0,0,0,0,0,0],"timestamps":[$(ts 15),$(ts 12),$(ts 9),$(ts 6),$(ts 3),$(ts 1)]}
{"metric":{"__name__":"meshmon_path_rtt_avg_micros","source":"fra-01","target":"nrt-01","protocol":"tcp"},"values":[278000,277000,279000,278500],"timestamps":[$(ts 8),$(ts 6),$(ts 4),$(ts 1)]}
{"metric":{"__name__":"meshmon_path_failure_rate","source":"fra-01","target":"nrt-01","protocol":"tcp"},"values":[0,0,0,0],"timestamps":[$(ts 8),$(ts 6),$(ts 4),$(ts 1)]}
NDJSON

# VM buffers writes for a few seconds before making them queryable. Flush
# so the service sees the samples immediately on first render.
curl -fsS -XPOST "http://127.0.0.1:${VM_PORT}/internal/force_flush" >/dev/null || true

# ---- Service (background) ----------------------------------------------
echo "[smoke] starting service on :${SERVICE_PORT} (log: ${SERVICE_LOG})"
MESHMON_CONFIG="$CONFIG_PATH" cargo run --quiet --package meshmon-service >"$SERVICE_LOG" 2>&1 &
SERVICE_PID=$!

echo "[smoke] waiting for service to be ready"
until curl -fs "http://127.0.0.1:${SERVICE_PORT}/readyz" >/dev/null 2>&1; do
  if ! kill -0 "$SERVICE_PID" 2>/dev/null; then
    echo "[smoke] service exited unexpectedly; tail of log:"
    tail -20 "$SERVICE_LOG"
    exit 1
  fi
  sleep 0.5
done

# ---- Frontend node_modules ---------------------------------------------
echo "[smoke] installing frontend dependencies"
npm --prefix frontend install

# ---- Frontend dev server (foreground) ----------------------------------
cat <<EOF

[smoke] infra ready
  Postgres:          127.0.0.1:${DB_PORT}   (user: meshmon, db: meshmon)
  VictoriaMetrics:   127.0.0.1:${VM_PORT}
  Alertmanager:      127.0.0.1:${AM_PORT}   (sub-path: /alertmanager)
  Grafana:           127.0.0.1:${GRAFANA_PORT}   (sub-path: /grafana)
  Service:           127.0.0.1:${SERVICE_PORT}   (log: ${SERVICE_LOG})
  Config:            ${CONFIG_PATH}

Open http://127.0.0.1:${FRONTEND_PORT}/ and log in as:
  username: ${ADMIN_USER}
  password: ${ADMIN_PASSWORD}

Ctrl-C tears everything down (service + containers).

EOF

export MESHMON_API_PROXY_TARGET="http://127.0.0.1:${SERVICE_PORT}"
exec npm --prefix frontend run dev -- --host 127.0.0.1 --port "$FRONTEND_PORT"

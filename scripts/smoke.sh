#!/usr/bin/env bash
# Local smoke-test harness for the meshmon service + frontend.
#
# Spins up Postgres + VictoriaMetrics containers, seeds a handful of agents
# and route snapshots, writes a config with a throwaway admin user, and then
# launches the service in the foreground. Ctrl-C tears down the containers.
#
# Not for production. For the full stack (vmalert, alertmanager, grafana)
# see deploy/docker-compose.yml once T24 fills it in.

set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT"

DB_CONTAINER=meshmon-smoke-db
VM_CONTAINER=meshmon-smoke-vm
DB_PORT=${MESHMON_SMOKE_DB_PORT:-5432}
VM_PORT=${MESHMON_SMOKE_VM_PORT:-8428}
SERVICE_PORT=${MESHMON_SMOKE_SERVICE_PORT:-8080}
CONFIG_PATH=${MESHMON_SMOKE_CONFIG:-/tmp/meshmon-smoke.toml}
ADMIN_USER=${MESHMON_SMOKE_USER:-admin}
ADMIN_PASSWORD=${MESHMON_SMOKE_PASSWORD:-smoketest}

TIMESCALE_IMAGE=timescale/timescaledb:2.26.3-pg16
VM_IMAGE=victoriametrics/victoria-metrics:v1.104.0

teardown() {
  echo
  echo "[smoke] tearing down containers"
  docker rm -f "$DB_CONTAINER" >/dev/null 2>&1 || true
  docker rm -f "$VM_CONTAINER" >/dev/null 2>&1 || true
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

# ---- Postgres -----------------------------------------------------------
docker rm -f "$DB_CONTAINER" >/dev/null 2>&1 || true
docker run --rm -d --name "$DB_CONTAINER" \
  -e POSTGRES_USER=meshmon -e POSTGRES_PASSWORD=meshmon -e POSTGRES_DB=meshmon \
  -p "${DB_PORT}:5432" "$TIMESCALE_IMAGE" >/dev/null

echo "[smoke] waiting for Postgres on :${DB_PORT}"
until docker exec "$DB_CONTAINER" pg_isready -U meshmon >/dev/null 2>&1; do
  sleep 0.5
done

# ---- VictoriaMetrics ----------------------------------------------------
docker rm -f "$VM_CONTAINER" >/dev/null 2>&1 || true
docker run --rm -d --name "$VM_CONTAINER" \
  -p "${VM_PORT}:8428" "$VM_IMAGE" >/dev/null

echo "[smoke] waiting for VictoriaMetrics on :${VM_PORT}"
until curl -fs "http://127.0.0.1:${VM_PORT}/health" >/dev/null 2>&1; do
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
PGPASSWORD=meshmon psql -h 127.0.0.1 -p "$DB_PORT" -U meshmon -d meshmon -v ON_ERROR_STOP=1 >/dev/null <<'SQL'
INSERT INTO agents (id, display_name, location, ip, lat, lon, agent_version, registered_at, last_seen_at, tcp_probe_port, udp_probe_port)
VALUES
  ('fra-01', 'Frankfurt 01', 'Frankfurt, DE', '10.10.0.1', 50.11,   8.68, '0.1.0', now() - interval '1 day', now(),                         7676, 7677),
  ('lon-01', 'London 01',    'London, UK',    '10.10.0.2', 51.51,  -0.13, '0.1.0', now() - interval '1 day', now() - interval '1 minute',   7676, 7677),
  ('nrt-01', 'Tokyo 01',     'Tokyo, JP',     '10.10.0.3', 35.68, 139.69, '0.1.0', now() - interval '1 day', now() - interval '30 minutes', 7676, 7677)
ON CONFLICT (id) DO NOTHING;

INSERT INTO route_snapshots (source_id, target_id, protocol, observed_at, hops, path_summary)
VALUES
  ('fra-01', 'lon-01', 'icmp', now() - interval '2 minutes',  '[]'::jsonb, NULL),
  ('lon-01', 'nrt-01', 'udp',  now() - interval '5 minutes',  '[]'::jsonb, NULL),
  ('fra-01', 'nrt-01', 'tcp',  now() - interval '10 minutes', '[]'::jsonb, NULL);
SQL

# ---- Service ------------------------------------------------------------
cat <<EOF

[smoke] infra ready
  Postgres:          127.0.0.1:${DB_PORT}   (user: meshmon, db: meshmon)
  VictoriaMetrics:   127.0.0.1:${VM_PORT}
  Service config:    ${CONFIG_PATH}

Open http://127.0.0.1:${SERVICE_PORT}/ and log in as:
  username: ${ADMIN_USER}
  password: ${ADMIN_PASSWORD}

Ctrl-C tears everything down.

EOF

exec env MESHMON_CONFIG="$CONFIG_PATH" cargo run --package meshmon-service

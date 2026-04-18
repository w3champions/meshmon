#!/usr/bin/env bash
# scripts/dev.sh — developer workflow for meshmon.
#
# Brings up the bundled infra (Postgres + VM + Grafana + AM + vmalert)
# via deploy/docker-compose.dev.yml, seeds a handful of agents and
# route snapshots, writes a throwaway meshmon.toml pointing at the
# exposed ports, starts `cargo run -p meshmon-service` in the
# background, and runs the Vite dev server in the foreground. Ctrl-C
# tears everything down.
#
# Not for production. For the full stack (including the service
# container built from Dockerfile.service), run the production compose:
#
#   cd deploy && docker compose up -d
set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT"

DEPLOY_DIR=deploy
ADMIN_PASSWORD=${MESHMON_DEV_ADMIN_PASSWORD:-smoketest}
PG_PASSWORD=${MESHMON_DEV_PG_PASSWORD:-meshmon}
PG_GRAFANA_PASSWORD=${MESHMON_DEV_PG_GRAFANA_PASSWORD:-grafana}
AGENT_TOKEN=${MESHMON_DEV_AGENT_TOKEN:-dev-token-0123456789}
# Non-standard default port to avoid clashing with Docker Desktop's
# common :8080 binding.
SERVICE_PORT=${MESHMON_DEV_SERVICE_PORT:-18080}

cleanup() {
    local rc=$?
    set +e
    if [[ -n "${SERVICE_PID:-}" ]]; then
        kill "$SERVICE_PID" 2>/dev/null || true
        wait "$SERVICE_PID" 2>/dev/null || true
    fi
    if [[ "${KEEP_INFRA:-0}" != "1" ]]; then
        (cd "$DEPLOY_DIR" && docker compose \
            -f docker-compose.yml -f docker-compose.dev.yml down -v) || true
        rm -f "$DEPLOY_DIR/.env" "$DEPLOY_DIR/meshmon.toml"
    else
        echo "[dev.sh] KEEP_INFRA=1 — leaving infra containers running" >&2
    fi
    exit "$rc"
}
trap cleanup EXIT INT TERM

echo "[dev.sh] staging deploy/.env + meshmon.toml"
ADMIN_HASH=$(echo -n "$ADMIN_PASSWORD" | argon2 "$(openssl rand -base64 16)" -id -t 2 -m 19 -p 1 -e)
# Compose interpolates '$' in .env values, so Argon2's '$' separators
# need to be doubled before they land in the file. We keep the
# un-escaped hash in ADMIN_HASH for use when we export the env var
# directly to `cargo run` below.
ADMIN_HASH_ESCAPED=${ADMIN_HASH//\$/\$\$}

UDP_PROBE_SECRET="hex:$(openssl rand -hex 8)"

cat > "$DEPLOY_DIR/.env" <<EOF
MESHMON_ADMIN_PASSWORD_HASH=$ADMIN_HASH_ESCAPED
MESHMON_AGENT_TOKEN=$AGENT_TOKEN
MESHMON_PG_PASSWORD=$PG_PASSWORD
MESHMON_PG_GRAFANA_PASSWORD=$PG_GRAFANA_PASSWORD
MESHMON_UDP_PROBE_SECRET=$UDP_PROBE_SECRET
# Compose rejects stacks when a secret's source env var is unset; empty
# values produce empty secret files inside Alertmanager (loud delivery
# errors rather than silent drops).
MESHMON_DISCORD_WEBHOOK=
MESHMON_DISCORD_WEBHOOK_CRITICAL=
MESHMON_DISCORD_WEBHOOK_INFO=
EOF

# Write a throwaway meshmon.toml from scratch (do NOT copy from
# meshmon.example.toml — that file already declares [upstream] and
# [[auth.users]], which would collide with the dev-specific values
# below). Keep this minimal and self-contained.
cat > "$DEPLOY_DIR/meshmon.toml" <<EOF
# dev.sh throwaway config — regenerated on every run. Do not commit.

[service]
listen_addr = "0.0.0.0:${SERVICE_PORT}"
# Dev server is plain HTTP; browsers drop Secure cookies on HTTP.
session_cookie_secure = false

[database]
url_env = "MESHMON_POSTGRES_URL"

[agent_api]
shared_token_env = "MESHMON_AGENT_TOKEN"

[[auth.users]]
username = "admin"
password_hash_env = "MESHMON_ADMIN_PASSWORD_HASH"

[upstream]
vm_url = "http://127.0.0.1:8428"
alertmanager_url = "http://127.0.0.1:9093/alertmanager"
grafana_url = "http://127.0.0.1:3000/grafana"

[probing]
udp_probe_secret_env = "MESHMON_UDP_PROBE_SECRET"
EOF

echo "[dev.sh] starting infra via docker compose dev overlay"
(cd "$DEPLOY_DIR" && docker compose \
    -f docker-compose.yml -f docker-compose.dev.yml up -d --wait)

echo "[dev.sh] waiting for Postgres"
until docker exec meshmon-db pg_isready -U meshmon -d meshmon >/dev/null 2>&1; do
    sleep 1
done

# Apply migrations before seeding; the dev overlay skips meshmon-service
# (profiles: ["skip"]) so no other process has created the schema yet.
echo "[dev.sh] applying migrations"
DATABASE_URL="postgres://meshmon:$PG_PASSWORD@127.0.0.1:5432/meshmon?sslmode=disable" \
    sqlx migrate run --source crates/service/migrations >/dev/null

echo "[dev.sh] seeding agents + route snapshots"
docker exec -e PGPASSWORD="$PG_PASSWORD" meshmon-db \
    psql -U meshmon -d meshmon -c "$(cat <<'SQL'
INSERT INTO agents (id, display_name, location, ip, lat, lon, last_seen_at, agent_version, tcp_probe_port, udp_probe_port)
VALUES
    ('dev-a', 'Dev A', 'Local', '10.0.0.1', 0, 0, now(), 'dev', 3555, 3552),
    ('dev-b', 'Dev B', 'Local', '10.0.0.2', 0, 0, now(), 'dev', 3555, 3552)
ON CONFLICT (id) DO UPDATE SET last_seen_at = now();
SQL
)"

echo "[dev.sh] seeding synthetic metric series into VM"
curl -s -X POST "http://127.0.0.1:8428/api/v1/import/prometheus" --data-binary @- <<'EOF'
meshmon_path_rtt_avg_micros{source="dev-a",target="dev-b",protocol="icmp"} 12000
meshmon_path_failure_rate{source="dev-a",target="dev-b",protocol="icmp"} 0.01
EOF

echo "[dev.sh] starting meshmon-service on :$SERVICE_PORT"
export MESHMON_CONFIG="$DEPLOY_DIR/meshmon.toml"
export MESHMON_AGENT_TOKEN="$AGENT_TOKEN"
export MESHMON_ADMIN_PASSWORD_HASH="$ADMIN_HASH"
export MESHMON_PG_GRAFANA_PASSWORD="$PG_GRAFANA_PASSWORD"
export MESHMON_UDP_PROBE_SECRET="$UDP_PROBE_SECRET"
export MESHMON_POSTGRES_URL="postgres://meshmon:$PG_PASSWORD@127.0.0.1:5432/meshmon?sslmode=disable"
export RUST_LOG="${RUST_LOG:-meshmon_service=debug,info}"
# Port comes from the throwaway toml's `service.listen_addr` above
# (0.0.0.0:$SERVICE_PORT). No env-var override for listen_addr exists;
# edit SERVICE_PORT at the top of this script if you need a different
# port.

cargo run -p meshmon-service &
SERVICE_PID=$!

echo "[dev.sh] service PID $SERVICE_PID; login as admin / $ADMIN_PASSWORD"
echo "[dev.sh] starting Vite dev server (Ctrl-C tears down everything)"
cd frontend
npm install
MESHMON_API_PROXY_TARGET="http://127.0.0.1:$SERVICE_PORT" npm run dev

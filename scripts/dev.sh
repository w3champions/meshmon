#!/usr/bin/env bash
# scripts/dev.sh — developer workflow for meshmon.
#
# Brings up the bundled infra (Postgres + VM + Grafana + AM + vmalert)
# via deploy/docker-compose.dev.yml, writes a throwaway meshmon.toml
# pointing at the exposed ports, starts `cargo run -p meshmon-service`
# in the background, and spawns 3 dev agents (docker-compose.agents-dev.yml)
# on a bridge network so the campaigns UI and multi-agent mesh flows are
# exercisable end-to-end. Finally runs the Vite dev server in the
# foreground. Ctrl-C tears everything down.
#
# Set MESHMON_DEV_SKIP_AGENTS=1 to skip the 3-agent overlay (useful when
# iterating on frontend-only changes — saves the first-run Dockerfile.agent
# build).
#
# Not for production. For the full stack (including the service
# container built from Dockerfile.service), run the production compose:
#
#   cd deploy && docker compose up -d
set -euo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT"

DEPLOY_DIR="$REPO_ROOT/deploy"
ADMIN_PASSWORD=${MESHMON_DEV_ADMIN_PASSWORD:-smoketest}
PG_PASSWORD=${MESHMON_DEV_PG_PASSWORD:-meshmon}
PG_GRAFANA_PASSWORD=${MESHMON_DEV_PG_GRAFANA_PASSWORD:-grafana}
AGENT_TOKEN=${MESHMON_DEV_AGENT_TOKEN:-dev-token-0123456789}
# Non-standard default port to avoid clashing with Docker Desktop's
# common :8080 binding.
SERVICE_PORT=${MESHMON_DEV_SERVICE_PORT:-18322}

cleanup() {
    local rc=$?
    set +e
    if [[ -n "${SERVICE_PID:-}" ]]; then
        kill "$SERVICE_PID" 2>/dev/null || true
        wait "$SERVICE_PID" 2>/dev/null || true
    fi
    if [[ "${KEEP_INFRA:-0}" != "1" ]]; then
        if [[ "${MESHMON_DEV_SKIP_AGENTS:-0}" != "1" ]]; then
            (cd "$DEPLOY_DIR" && docker compose \
                -f docker-compose.agents-dev.yml down -v) || true
        fi
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
# Consumed by docker-compose.agents-dev.yml so the dev agents can reach
# the host-side meshmon-service via host.docker.internal.
MESHMON_DEV_SERVICE_PORT=$SERVICE_PORT
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

# Opt-in: append the ipgeolocation.io block when the API key is exported in
# the calling shell. Keeping it out of the main heredoc avoids parameter
# expansion stripping the inner quotes on `api_key_env = "..."`.
if [[ -n "${MESHMON_IPGEO_API_KEY:-}" ]]; then
    cat >> "$DEPLOY_DIR/meshmon.toml" <<'EOF'

[enrichment.ipgeolocation]
enabled = true
acknowledged_tos = true
api_key_env = "MESHMON_IPGEO_API_KEY"
EOF
fi

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

echo "[dev.sh] starting meshmon-service on :$SERVICE_PORT"
export MESHMON_CONFIG="$DEPLOY_DIR/meshmon.toml"
export MESHMON_AGENT_TOKEN="$AGENT_TOKEN"
export MESHMON_ADMIN_PASSWORD_HASH="$ADMIN_HASH"
export MESHMON_PG_GRAFANA_PASSWORD="$PG_GRAFANA_PASSWORD"
export MESHMON_UDP_PROBE_SECRET="$UDP_PROBE_SECRET"
export MESHMON_POSTGRES_URL="postgres://meshmon:$PG_PASSWORD@127.0.0.1:5432/meshmon?sslmode=disable"
# Forward the ipgeolocation.io key when present so the service picks it up via
# `api_key_env`. Unset by default — export it in your shell (do NOT commit).
if [[ -n "${MESHMON_IPGEO_API_KEY:-}" ]]; then
    export MESHMON_IPGEO_API_KEY
    echo "[dev.sh] ipgeolocation provider enabled (MESHMON_IPGEO_API_KEY set)"
else
    echo "[dev.sh] ipgeolocation disabled — export MESHMON_IPGEO_API_KEY to enable"
fi
export RUST_LOG="${RUST_LOG:-meshmon_service=debug,info}"
# Port comes from the throwaway toml's `service.listen_addr` above
# (0.0.0.0:$SERVICE_PORT). No env-var override for listen_addr exists;
# edit SERVICE_PORT at the top of this script if you need a different
# port.

cargo run -p meshmon-service &
SERVICE_PID=$!

echo "[dev.sh] service PID $SERVICE_PID; login as admin / $ADMIN_PASSWORD"

# Wait for the service to come up before firing up the dev agents —
# agents hit /healthz-adjacent gRPC on first boot and would burn through
# their retry budget if the service hasn't opened the port yet.
echo "[dev.sh] waiting for meshmon-service /healthz"
service_ready=0
for _ in $(seq 1 60); do
    if curl -sfo /dev/null "http://127.0.0.1:$SERVICE_PORT/healthz"; then
        service_ready=1
        break
    fi
    sleep 1
done
if [[ "$service_ready" != "1" ]]; then
    echo "[dev.sh] WARNING: /healthz did not return 200 within 60s; continuing" >&2
fi

if [[ "${MESHMON_DEV_SKIP_AGENTS:-0}" != "1" ]]; then
    echo "[dev.sh] starting 3 dev agents on bridge 172.31.0.0/24 (cold Dockerfile.agent build can take several minutes)"
    (cd "$DEPLOY_DIR" && docker compose \
        -f docker-compose.agents-dev.yml up -d --build)
    echo "[dev.sh] 3 dev agents registering from 172.31.0.11/12/13 (Frankfurt, São Paulo, Singapore)"
else
    echo "[dev.sh] MESHMON_DEV_SKIP_AGENTS=1 — skipping dev agents"
fi

echo "[dev.sh] starting Vite dev server (Ctrl-C tears down everything)"
cd frontend
npm install
MESHMON_API_PROXY_TARGET="http://127.0.0.1:$SERVICE_PORT" npm run dev

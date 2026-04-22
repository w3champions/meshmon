#!/usr/bin/env bash
# scripts/dev.sh — developer workflow for meshmon.
#
# Brings up the bundled infra (Postgres + VM + Grafana + AM + vmalert)
# via deploy/docker-compose.dev.yml, writes a throwaway meshmon.toml
# pointing at the exposed ports, and spawns 3 dev agents
# (docker-compose.agents-dev.yml) on a bridge network so the campaigns
# UI and multi-agent mesh flows are exercisable end-to-end.
#
# When tmux is available (default), the script drops into a 3-pane
# tmux session named "meshmon-dev":
#
#   +--------------------+--------------------------+
#   |                    |  Vite dev server         |
#   |  cargo run         |  (top-right)             |
#   |  meshmon-service   +--------------------------+
#   |  (left)            |  docker compose logs -f  |
#   |                    |  agents (bottom-right)   |
#   +--------------------+--------------------------+
#
# Detach with the tmux prefix + d; Ctrl-C in the parent shell tears
# everything down. Set MESHMON_DEV_TMUX=0 to bypass tmux entirely — the
# service is then backgrounded with logs redirected to
# target/dev-logs/meshmon-service.log and Vite runs in the foreground
# (the legacy flow).
#
# If $TMUX is set (already inside a tmux session), a new window is
# opened inside the current session rather than nesting a new session —
# cleanup kills that window on exit.
#
# Set MESHMON_DEV_SKIP_AGENTS=1 to skip the 3-agent overlay (useful when
# iterating on frontend-only changes — saves the first-run Dockerfile.agent
# build). In tmux mode this also drops the bottom-right logs pane, so
# Vite takes the full-height right column.
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

TMUX_SESSION="meshmon-dev"
# TMUX_WINDOW is set only in nested-tmux mode (choice (a)) — see below.
TMUX_WINDOW=""
SERVICE_LOG="$REPO_ROOT/target/dev-logs/meshmon-service.log"

cleanup() {
    local rc=$?
    set +e
    # Disarm the trap so the bash-level exit path doesn't re-enter
    # cleanup while we're still running (HUP during docker-compose down
    # would otherwise re-fire this function and race with itself).
    trap - EXIT INT TERM HUP
    echo "[dev.sh] cleanup starting (exit status $rc)" >&2
    # Kill tmux surface first so pane-hosted processes (cargo, vite,
    # docker compose logs) receive SIGHUP before the containers they
    # watch go away. Both branches are idempotent.
    if [[ -n "$TMUX_WINDOW" ]]; then
        echo "[dev.sh] cleanup: tmux kill-window $TMUX_WINDOW" >&2
        tmux kill-window -t "$TMUX_WINDOW" 2>/dev/null || true
    fi
    tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true
    if [[ -n "${SERVICE_PID:-}" ]]; then
        echo "[dev.sh] cleanup: killing meshmon-service PID $SERVICE_PID" >&2
        kill "$SERVICE_PID" 2>/dev/null || true
        wait "$SERVICE_PID" 2>/dev/null || true
    fi
    if [[ "${KEEP_INFRA:-0}" != "1" ]]; then
        if [[ "${MESHMON_DEV_SKIP_AGENTS:-0}" != "1" ]]; then
            echo "[dev.sh] cleanup: docker compose down dev agents" >&2
            (cd "$DEPLOY_DIR" && docker compose \
                -f docker-compose.agents-dev.yml down -v) || true
        fi
        echo "[dev.sh] cleanup: docker compose down infra (postgres / VM / grafana / alertmanager / vmalert)" >&2
        (cd "$DEPLOY_DIR" && docker compose \
            -f docker-compose.yml -f docker-compose.dev.yml down -v) || true
        echo "[dev.sh] cleanup: removing throwaway $DEPLOY_DIR/.env and meshmon.toml" >&2
        rm -f "$DEPLOY_DIR/.env" "$DEPLOY_DIR/meshmon.toml"
    else
        echo "[dev.sh] KEEP_INFRA=1 — leaving infra containers running" >&2
    fi
    echo "[dev.sh] cleanup done" >&2
    exit "$rc"
}
trap cleanup EXIT INT TERM HUP

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

[logging]
# Compact tracing lines beat JSON for reading in a tmux pane; prod
# deploys override this via meshmon.example.toml.
format = "compact"

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

# Defensive: fail fast if something else already owns the service port.
# Most commonly a stray `cargo run` from a prior dev.sh that didn't
# fully clean up — cargo would then bind-fail inside the tmux pane and
# the pane would close before you could read the error.
if lsof -iTCP:"$SERVICE_PORT" -sTCP:LISTEN -n -P 2>/dev/null | grep -q LISTEN; then
    echo "[dev.sh] ERROR: port $SERVICE_PORT is already in use:" >&2
    lsof -iTCP:"$SERVICE_PORT" -sTCP:LISTEN -n -P >&2 || true
    echo "[dev.sh]   kill the offender (\`lsof -ti :$SERVICE_PORT | xargs -r kill -9\`) or override MESHMON_DEV_SERVICE_PORT." >&2
    exit 1
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

# Decide whether to use tmux for multi-pane output. See the header
# comment for the full matrix; nested tmux uses choice (a) — open a new
# window inside the existing session and kill that window on cleanup.
USE_TMUX=1
NESTED_TMUX=0
if [[ "${MESHMON_DEV_TMUX:-1}" == "0" ]]; then
    USE_TMUX=0
elif ! command -v tmux >/dev/null 2>&1; then
    echo "[dev.sh] tmux not found — falling back to single-terminal mode; service logs → $SERVICE_LOG" >&2
    USE_TMUX=0
elif [[ -n "${TMUX:-}" ]]; then
    NESTED_TMUX=1
fi

if [[ "$USE_TMUX" == "1" ]]; then
    # Make sure no stale session from a previous run is left behind.
    tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true

    # Shell-fragment helpers. The service and vite commands do NOT exec;
    # instead they capture the child's exit code and pause on a keypress
    # so a boot-time failure (config error, port collision, compile
    # failure) remains on-screen for diagnosis instead of closing the
    # pane instantly. The agent-logs pane runs `docker compose logs -f`
    # which only exits on user Ctrl-C, so exec is fine there.
    # shellcheck disable=SC2016  # intentional: $? and $rc expand in the pane shell, not here.
    PAUSE_ON_EXIT='rc=$?; echo; echo "[dev.sh] pane command exited with status $rc"; echo "[dev.sh] press any key to close this pane"; read -n 1 -s -r'
    SERVICE_CMD="cd $(printf '%q' "$REPO_ROOT") && cargo run -p meshmon-service; $PAUSE_ON_EXIT"
    VITE_CMD="cd $(printf '%q' "$REPO_ROOT/frontend") && MESHMON_API_PROXY_TARGET=http://127.0.0.1:$SERVICE_PORT npm run dev; $PAUSE_ON_EXIT"
    AGENT_LOGS_CMD="cd $(printf '%q' "$DEPLOY_DIR") && exec docker compose -f docker-compose.agents-dev.yml logs -f"

    # When a tmux server is already running, new sessions/windows inherit
    # the server's environment (from its first invocation), NOT the
    # current shell's. Without forwarding, the service pane boots with
    # no MESHMON_* env vars and cargo-run exits immediately on config
    # load. Pass every required var via -e so the pane's cargo invocation
    # sees them. Pane splits inherit the parent session/window env — no
    # need to duplicate -e on split-window.
    TMUX_ENV_ARGS=(
        -e "MESHMON_CONFIG=$MESHMON_CONFIG"
        -e "MESHMON_AGENT_TOKEN=$MESHMON_AGENT_TOKEN"
        -e "MESHMON_ADMIN_PASSWORD_HASH=$MESHMON_ADMIN_PASSWORD_HASH"
        -e "MESHMON_PG_GRAFANA_PASSWORD=$MESHMON_PG_GRAFANA_PASSWORD"
        -e "MESHMON_UDP_PROBE_SECRET=$MESHMON_UDP_PROBE_SECRET"
        -e "MESHMON_POSTGRES_URL=$MESHMON_POSTGRES_URL"
        -e "RUST_LOG=$RUST_LOG"
    )
    if [[ -n "${MESHMON_IPGEO_API_KEY:-}" ]]; then
        TMUX_ENV_ARGS+=(-e "MESHMON_IPGEO_API_KEY=$MESHMON_IPGEO_API_KEY")
    fi

    if [[ "$NESTED_TMUX" == "1" ]]; then
        # Choice (a): open a new window in the caller's existing session
        # rather than nesting sessions. Capture the window id so cleanup
        # can target it precisely.
        TMUX_WINDOW=$(tmux new-window -P -F '#{session_name}:#{window_id}' \
            "${TMUX_ENV_ARGS[@]}" \
            -n "$TMUX_SESSION" "bash -lc $(printf '%q' "$SERVICE_CMD")")
        echo "[dev.sh] opened tmux window $TMUX_WINDOW for meshmon-service (nested tmux mode)"
    else
        tmux new-session -d -s "$TMUX_SESSION" -n main \
            "${TMUX_ENV_ARGS[@]}" \
            "bash -lc $(printf '%q' "$SERVICE_CMD")"
    fi
fi

if [[ "$USE_TMUX" != "1" ]]; then
    mkdir -p "$(dirname "$SERVICE_LOG")"
    echo "[dev.sh] service logs → $SERVICE_LOG (tail -F $SERVICE_LOG from another terminal)"
    # shellcheck disable=SC2024  # redirect is for cargo, not sudo
    cargo run -p meshmon-service >"$SERVICE_LOG" 2>&1 &
    SERVICE_PID=$!
    echo "[dev.sh] service PID $SERVICE_PID; login as admin / $ADMIN_PASSWORD"
else
    echo "[dev.sh] meshmon-service launching in tmux pane (session $TMUX_SESSION); login as admin / $ADMIN_PASSWORD"
fi

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

if [[ "$USE_TMUX" == "1" ]]; then
    echo "[dev.sh] installing frontend deps (npm install)"
    (cd "$REPO_ROOT/frontend" && npm install)

    # Target for splits: the service pane in either the detached
    # session's "main" window, or the window we just opened inside the
    # caller's session.
    if [[ "$NESTED_TMUX" == "1" ]]; then
        SPLIT_TARGET="$TMUX_WINDOW"
        tmux list-windows -F '#{window_id}' -t "${TMUX_WINDOW%%:*}" 2>/dev/null \
            | grep -qx "${TMUX_WINDOW##*:}" || {
            echo "[dev.sh] ERROR: tmux window $TMUX_WINDOW died before splits could be applied." >&2
            echo "[dev.sh]   Usually means the service pane exited on startup (config error, port in use, DB unreachable)." >&2
            echo "[dev.sh]   Re-run with MESHMON_DEV_TMUX=0 to see the error inline." >&2
            exit 1
        }
    else
        SPLIT_TARGET="$TMUX_SESSION:main"
        tmux has-session -t "$TMUX_SESSION" 2>/dev/null || {
            echo "[dev.sh] ERROR: tmux session $TMUX_SESSION died before splits could be applied." >&2
            echo "[dev.sh]   Usually means the service pane exited on startup (config error, port in use, DB unreachable)." >&2
            echo "[dev.sh]   Re-run with MESHMON_DEV_TMUX=0 to see the error inline." >&2
            exit 1
        }
    fi

    # Top-right: Vite. Split horizontally (new pane to the right), 50%.
    tmux split-window -h -t "$SPLIT_TARGET" -p 50 \
        "bash -lc $(printf '%q' "$VITE_CMD")"

    if [[ "${MESHMON_DEV_SKIP_AGENTS:-0}" != "1" ]]; then
        # Bottom-right: agent container logs. Split the Vite pane
        # vertically; -p 40 so the logs pane is a bit shorter than Vite's.
        tmux split-window -v -p 40 \
            "bash -lc $(printf '%q' "$AGENT_LOGS_CMD")"
    fi

    tmux select-pane -t "$SPLIT_TARGET"

    echo "[dev.sh] tmux layout: cargo logs in left pane, Vite in top-right, agents in bottom-right; prefix-d to detach"
    echo "[dev.sh] login as admin / $ADMIN_PASSWORD"
    if [[ "$NESTED_TMUX" == "1" ]]; then
        tmux select-window -t "$TMUX_WINDOW"
        # Nested mode: caller is already attached to the session, so we
        # just block until the window closes. A simple wait loop is
        # enough — cleanup handles the teardown regardless.
        while tmux list-windows -F '#{window_id}' \
            -t "${TMUX_WINDOW%%:*}" 2>/dev/null | grep -qx "${TMUX_WINDOW##*:}"; do
            sleep 1
        done
    else
        tmux attach-session -t "$TMUX_SESSION"
    fi

    # attach returned (user detached or session died). Clean up
    # proactively — the EXIT trap below will no-op on the second kill.
    if [[ -n "$TMUX_WINDOW" ]]; then
        tmux kill-window -t "$TMUX_WINDOW" 2>/dev/null || true
    fi
    tmux kill-session -t "$TMUX_SESSION" 2>/dev/null || true
else
    echo "[dev.sh] starting Vite dev server (Ctrl-C tears down everything)"
    echo "[dev.sh] follow service logs with: tail -F $SERVICE_LOG"
    cd frontend
    npm install
    MESHMON_API_PROXY_TARGET="http://127.0.0.1:$SERVICE_PORT" npm run dev
fi

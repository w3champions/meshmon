#!/usr/bin/env bash
# smoke-dashboards.sh — end-to-end dashboard smoke test.
#
# 1. Materialize grafana/test-harness/provisioning/datasources/meshmon.yml
#    from the canonical grafana/provisioning/datasources.yml.template via
#    envsubst — harness-specific URLs are filled in here, template remains
#    the single source of truth.
# 2. Spin up VM + Grafana via the test-harness compose.
# 3. Seed VM with a synthetic meshmon_path_* sample so /d-solo panels have
#    data to render.
# 4. GET /d-solo/<uid>?panelId=1&kiosk for each dashboard; assert HTTP 200.
#
# Cleans up on exit (success or failure). Requires docker + docker compose v2
# + envsubst (from gettext).
set -euo pipefail

cd "$(dirname "$0")/.."

if [[ ! -f deploy/versions.env ]]; then
  echo "::error ::missing deploy/versions.env" >&2
  exit 1
fi

set -a
. ./deploy/versions.env
set +a

: "${VM_TAG:?VM_TAG must be set in deploy/versions.env}"
: "${GRAFANA_TAG:?GRAFANA_TAG must be set in deploy/versions.env}"

command -v envsubst >/dev/null 2>&1 || {
  echo "::error ::envsubst not found (install gettext)" >&2
  exit 1
}

DS_DIR="grafana/test-harness/provisioning/datasources"
mkdir -p "$DS_DIR"
DS_YAML="$DS_DIR/meshmon.yml"

# Fill in harness-internal URLs against the canonical template.
MESHMON_VM_URL="http://meshmon-vm:8428" \
MESHMON_PG_URL="" \
MESHMON_PG_USER="" \
MESHMON_PG_PASSWORD="" \
MESHMON_PG_DATABASE="" \
envsubst < grafana/provisioning/datasources.yml.template > "$DS_YAML"

# Postgres isn't part of the smoke harness (no dashboard queries Postgres),
# so strip the MeshmonPostgres datasource block from the generated file.
# Simpler than adding conditionals to the template.
node -e "
const fs=require('node:fs');
const yaml=fs.readFileSync(process.argv[1],'utf8');
const out=yaml.replace(/\n  - name: MeshmonPostgres[\s\S]*?(?=\n\w|$)/,'');
fs.writeFileSync(process.argv[1], out);
" "$DS_YAML"

COMPOSE=(docker compose -f grafana/test-harness/docker-compose.yml --env-file deploy/versions.env)

cleanup() {
  "${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
  rm -f "$DS_YAML"
}
trap cleanup EXIT

echo "==> bringing up VM + Grafana (tags: VM=${VM_TAG}, Grafana=${GRAFANA_TAG})"
"${COMPOSE[@]}" up -d

echo "==> waiting for Grafana healthcheck"
ok=false
for i in {1..60}; do
  body=$(curl -fsS -u admin:admin http://127.0.0.1:3000/api/health 2>/dev/null || true)
  if [[ -n "$body" ]] && node -e "const h=JSON.parse(process.argv[1]); if (h.database!=='ok') process.exit(1);" "$body" 2>/dev/null; then
    ok=true
    break
  fi
  sleep 1
done
if ! $ok; then
  echo "::error ::Grafana failed to become healthy in 60 s" >&2
  "${COMPOSE[@]}" logs meshmon-grafana >&2 || true
  exit 1
fi

echo "==> seeding VM with synthetic meshmon_path_* samples"
NOW=$(date +%s)
for offset in 180 120 60 0; do
  ts=$(( (NOW - offset) * 1000 ))
  curl -fsS -H 'Content-Type: text/plain' \
    --data-binary "@-" \
    "http://127.0.0.1:8428/api/v1/import/prometheus?timestamp=${ts}" <<EOF
meshmon_path_rtt_avg_micros{source="agent-a",target="agent-b",protocol="icmp"} $((RANDOM % 500 + 500))
meshmon_path_rtt_min_micros{source="agent-a",target="agent-b",protocol="icmp"} $((RANDOM % 200 + 300))
meshmon_path_rtt_max_micros{source="agent-a",target="agent-b",protocol="icmp"} $((RANDOM % 800 + 700))
meshmon_path_rtt_stddev_micros{source="agent-a",target="agent-b",protocol="icmp"} $((RANDOM % 100 + 50))
meshmon_path_failure_rate{source="agent-a",target="agent-b",protocol="icmp"} 0.01
meshmon_agent_info{source="agent-a",agent_version="0.1.0"} 1
meshmon_agent_info{source="agent-b",agent_version="0.1.0"} 1
meshmon_agent_last_seen_seconds{source="agent-a"} ${NOW}
meshmon_agent_last_seen_seconds{source="agent-b"} ${NOW}
EOF
done

echo "==> asserting /d-solo iframes return 200 (GET with status code capture)"
FROM=$(( (NOW - 3600) * 1000 ))
TO=$(( NOW * 1000 ))

assert_200() {
  local url=$1 label=$2
  local code
  code=$(curl -fsS -o /dev/null -w '%{http_code}' -u admin:admin "$url" || true)
  if [[ "$code" != "200" ]]; then
    echo "::error ::$label returned HTTP $code (expected 200)" >&2
    echo "   URL: $url" >&2
    return 1
  fi
  echo "  OK 200: $label"
}

# Lookup is UID-only; the slug segment after /d-solo/<uid>/ is cosmetic.
# Pass a stable no-op slug so URL shape matches the frontend's builder.
assert_200 "http://127.0.0.1:3000/d-solo/meshmon-path/path?panelId=1&var-source=agent-a&var-target=agent-b&var-protocol=icmp&from=${FROM}&to=${TO}&theme=light&kiosk" \
  "path: RTT"
assert_200 "http://127.0.0.1:3000/d-solo/meshmon-path/path?panelId=2&var-source=agent-a&var-target=agent-b&var-protocol=icmp&from=${FROM}&to=${TO}&theme=light&kiosk" \
  "path: Loss"
assert_200 "http://127.0.0.1:3000/d-solo/meshmon-path/path?panelId=3&var-source=agent-a&var-target=agent-b&var-protocol=icmp&from=${FROM}&to=${TO}&theme=light&kiosk" \
  "path: Stddev"
assert_200 "http://127.0.0.1:3000/d-solo/meshmon-overview/overview?panelId=1&from=${FROM}&to=${TO}&kiosk" \
  "overview: heatmap"
assert_200 "http://127.0.0.1:3000/d-solo/meshmon-overview/overview?panelId=2&from=${FROM}&to=${TO}&kiosk" \
  "overview: degraded-paths stat"
assert_200 "http://127.0.0.1:3000/d-solo/meshmon-overview/overview?panelId=3&from=${FROM}&to=${TO}&kiosk" \
  "overview: route-changes table"
assert_200 "http://127.0.0.1:3000/d-solo/meshmon-agent/agent?panelId=1&var-source=agent-a&from=${FROM}&to=${TO}&kiosk" \
  "agent: outgoing RTT"
assert_200 "http://127.0.0.1:3000/d-solo/meshmon-agent/agent?panelId=4&var-source=agent-a&from=${FROM}&to=${TO}&kiosk" \
  "agent: incoming loss"

echo ""
echo "OK: every iframed panel returned 200"

#!/usr/bin/env bash
# End-to-end smoke: inject a high-loss sample into VM, wait for vmalert
# to fire, confirm Alertmanager dispatched to the webhook sink.
#
# NOT run in CI — docker-compose startup in GH runners is slow/flaky.
# For local manual verification and pre-merge hand-testing.
set -euo pipefail

cd "$(dirname "$0")/.."

[[ -f deploy/versions.env ]] || { echo "::error ::missing deploy/versions.env" >&2; exit 1; }

# --env-file makes the tags available for interpolation inside the compose
# file — no duplication between this script, the compose file, and the
# validator.
COMPOSE="docker compose --env-file deploy/versions.env -f deploy/alerts/test-harness/docker-compose.yml"
DATA_DIR="deploy/alerts/test-harness/data"

cleanup() {
  echo "==> Tearing down"
  $COMPOSE down -v --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "==> Cleaning up any previous harness containers"
$COMPOSE down -v --remove-orphans >/dev/null 2>&1 || true

# Pre-flight port checks — the harness binds 8428, 9093, 18080 on 127.0.0.1.
# If scripts/smoke.sh is running (meshmon-smoke-vm uses 8428) or another
# process holds 18080, bring those down first.
for port in 8428 9093 18080; do
  if lsof -ti :"$port" >/dev/null 2>&1; then
    echo "::warning ::port $port is in use — stop any competing smoke sessions (scripts/smoke.sh) before running this harness"
    lsof -ti :"$port" | xargs ps -p 2>/dev/null | tail -n +2 || true
    exit 1
  fi
done

echo "==> Starting harness (VM + VMAlert + Alertmanager + webhook sink)"
rm -rf "$DATA_DIR"
mkdir -p "$DATA_DIR"
$COMPOSE up -d --build

# VM readiness: /health returns 200. Allow up to 60 s for container pull + start.
echo "==> Waiting for VM ready"
vm_ready=0
for _ in {1..60}; do
  if curl -fsS http://127.0.0.1:8428/health >/dev/null 2>&1; then
    vm_ready=1
    break
  fi
  sleep 1
done
if [[ $vm_ready -eq 0 ]]; then
  echo "::error ::VM did not become ready within 60 s"
  $COMPOSE logs --tail=20 vm
  exit 1
fi
echo "==> VM ready"

# Portable millisecond timestamp: try GNU date first, fall back to python3.
now_ms() {
  if date +%s%3N 2>/dev/null | grep -qE '^[0-9]{13}$'; then
    date +%s%3N
  else
    python3 -c 'import time; print(int(time.time()*1000))'
  fi
}

# Inject a high-loss sample via Prometheus import API. VMAlert evaluates
# every 10 s; rule `for: 2m` means we keep pushing for > 2 min for
# PathPacketLoss to fire. Push a fresh sample every 5 s for 180 s.
echo "==> Injecting meshmon_path_failure_rate samples for 3 min"
for i in $(seq 1 36); do
  ts_ms=$(now_ms)
  # Allow individual injection failures (transient) — the loop keeps running.
  curl -fsS --data-binary @- http://127.0.0.1:8428/api/v1/import/prometheus <<EOF >/dev/null || true
meshmon_path_failure_rate{source="smoke-a",target="smoke-b",protocol="icmp"} 0.30 ${ts_ms}
EOF
  sleep 5
done

echo "==> Waiting up to 60 s for dispatch to webhook sink"
for _ in {1..60}; do
  if [[ -s "$DATA_DIR/received.jsonl" ]]; then
    break
  fi
  sleep 1
done

if ! [[ -s "$DATA_DIR/received.jsonl" ]]; then
  echo "::error ::no webhook dispatch received — check VMAlert and Alertmanager logs:"
  $COMPOSE logs --tail=50 vmalert alertmanager
  exit 1
fi

echo "==> Verifying dispatched payload references PathPacketLoss for smoke-a → smoke-b"
# PathPacketLossCritical (for: 1m, >0.20) fires first at 0.30 loss;
# PathPacketLoss (for: 2m, >0.05) follows. Accept either — both confirm
# the full dispatch chain (vmalert → alertmanager → webhook sink).
# The Python sink serialises JSON with spaces after colons/commas.
if ! grep -qE '"alertname": "PathPacketLoss(Critical)?"' "$DATA_DIR/received.jsonl" \
   || ! grep -q '"source": "smoke-a"' "$DATA_DIR/received.jsonl" \
   || ! grep -q '"target": "smoke-b"' "$DATA_DIR/received.jsonl"; then
  echo "::error ::dispatched payload did not contain expected labels"
  head -c 4096 "$DATA_DIR/received.jsonl"
  exit 1
fi

echo
echo "OK: end-to-end dispatch verified"

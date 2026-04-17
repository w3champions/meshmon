#!/usr/bin/env bash
# validate-dashboards.sh — Hermetic validation for Grafana dashboards.
#
#   1. JSON syntax check on every dashboards/*.json via node
#   2. verify-panels.mjs       — panels.json ⊆ dashboards/ contract
#
# No docker required. For an end-to-end dashboard-loads-in-real-grafana
# check, use scripts/smoke-dashboards.sh.
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> JSON syntax (grafana/dashboards/*.json)"
shopt -s nullglob
dashboards=(grafana/dashboards/*.json)
shopt -u nullglob

if [[ ${#dashboards[@]} -eq 0 ]]; then
  echo "::error ::no dashboards in grafana/dashboards/" >&2
  exit 1
fi

for f in "${dashboards[@]}"; do
  node -e "JSON.parse(require('node:fs').readFileSync(process.argv[1],'utf8'))" "$f" \
    || { echo "::error ::invalid JSON: $f" >&2; exit 1; }
  echo "  OK syntax: $f"
done

echo "==> verify-panels.mjs"
node grafana/verify-panels.mjs

echo ""
echo "OK: dashboards validated"

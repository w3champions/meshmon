#!/usr/bin/env bash
# validate-alerts.sh — Full validation pipeline for alert rules and
# Alertmanager config.  Runs four checks in sequence:
#
#   1. check-rule-metrics.sh  — metric name cross-check against service source
#   2. vmalert -dryRun        — rule-file YAML + PromQL syntax check
#   3. vmalert-tool unittest  — per-rule unit tests (one run per test file)
#   4. amtool check-config    — Alertmanager config validation
#
# Source deploy/versions.env before running any docker images so that the
# exact same tags used locally are used in CI.
set -euo pipefail

cd "$(dirname "$0")/.."

# ---------------------------------------------------------------------------
# 1. Load image tags from the single source of truth.
# ---------------------------------------------------------------------------
if [[ ! -f deploy/versions.env ]]; then
  echo "::error ::missing deploy/versions.env" >&2
  exit 1
fi

set -a
. ./deploy/versions.env
set +a

: "${VMALERT_TAG:?VMALERT_TAG must be set in deploy/versions.env}"
: "${VMALERT_TOOL_TAG:?VMALERT_TOOL_TAG must be set in deploy/versions.env}"
: "${ALERTMANAGER_TAG:?ALERTMANAGER_TAG must be set in deploy/versions.env}"

# ---------------------------------------------------------------------------
# 2. Metric-name cross-check (service source vs rules.yaml).
# ---------------------------------------------------------------------------
echo "==> check-rule-metrics"
bash scripts/check-rule-metrics.sh

# ---------------------------------------------------------------------------
# 3. vmalert -dryRun: rule-file YAML + PromQL syntax.
# ---------------------------------------------------------------------------
echo "==> vmalert -dryRun (image: victoriametrics/vmalert:${VMALERT_TAG})"
docker run --rm \
  -v "$PWD/deploy/alerts:/alerts:ro" \
  "victoriametrics/vmalert:${VMALERT_TAG}" \
  -dryRun -rule=/alerts/rules.yaml

# ---------------------------------------------------------------------------
# 4. vmalert-tool unittest: run each *_test.yaml under deploy/alerts/tests/.
# ---------------------------------------------------------------------------
echo "==> vmalert-tool unittest (image: victoriametrics/vmalert-tool:${VMALERT_TOOL_TAG})"
shopt -s nullglob
test_files=(deploy/alerts/tests/*_test.yaml)
shopt -u nullglob

if [[ ${#test_files[@]} -eq 0 ]]; then
  echo "  (no *_test.yaml files found — skipping unittest step)"
else
  for tf in "${test_files[@]}"; do
    name=$(basename "$tf")
    echo "  -- unittest: $name"
    docker run --rm \
      -v "$PWD/deploy:/deploy:ro" \
      "victoriametrics/vmalert-tool:${VMALERT_TOOL_TAG}" \
      unittest --files="/deploy/alerts/tests/${name}"
  done
fi

# ---------------------------------------------------------------------------
# 5. amtool check-config: Alertmanager config validation.
# ---------------------------------------------------------------------------
echo "==> amtool check-config (image: prom/alertmanager:${ALERTMANAGER_TAG})"
docker run --rm \
  -v "$PWD/deploy/alertmanager:/etc/alertmanager:ro" \
  --entrypoint /bin/amtool \
  "prom/alertmanager:${ALERTMANAGER_TAG}" \
  check-config /etc/alertmanager/alertmanager.yml

echo ""
echo "OK: alerts + alertmanager config validated"

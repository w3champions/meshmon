#!/usr/bin/env bash
# check-rule-metrics.sh — Verify every meshmon_* metric referenced in
# deploy/alerts/rules.yaml appears as a quoted string literal in the service
# source tree (crates/service/src/).
#
# Fails fast (exit 1) when any rule-referenced metric is absent from the
# source, printing a GitHub-Actions-compatible ::error:: banner plus the
# missing names.  The reverse direction is intentionally NOT enforced: the
# service may emit metrics that no alert rule currently uses.
set -euo pipefail

cd "$(dirname "$0")/.."

RULES="deploy/alerts/rules.yaml"
SRC_DIR="crates/service/src"

if [[ ! -f "$RULES" ]]; then
  echo "::error ::$RULES not found — nothing to check" >&2
  exit 1
fi

# Extract distinct meshmon_* metric names referenced in the rules file.
metrics=$(grep -oE 'meshmon_[a-z0-9_]+' "$RULES" | sort -u)

if [[ -z "$metrics" ]]; then
  echo "No meshmon_* metrics found in $RULES — skipping cross-check"
  exit 0
fi

missing=()
while IFS= read -r m; do
  if ! grep -qRF "\"$m\"" "$SRC_DIR"; then
    missing+=("$m")
  fi
done <<< "$metrics"

if [[ ${#missing[@]} -gt 0 ]]; then
  echo "::error ::rules.yaml references metrics not found in $SRC_DIR:" >&2
  for m in "${missing[@]}"; do
    echo "  - $m" >&2
  done
  exit 1
fi

echo "OK: all meshmon_* metrics in $RULES are present in $SRC_DIR"

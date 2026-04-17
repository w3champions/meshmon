#!/usr/bin/env bash
# Build a release meshmon-service binary with the real React SPA embedded.
# Equivalent to the CI release-binary job (see .github/workflows/ci.yml).
set -euo pipefail

cd "$(dirname "$0")/.."

echo "==> Building frontend"
pushd frontend >/dev/null
npm ci
npm run build
popd >/dev/null

echo "==> Building release binary"
cargo build --release -p meshmon-service

echo
echo "Binary: $(pwd)/target/release/meshmon-service"
ls -lh target/release/meshmon-service

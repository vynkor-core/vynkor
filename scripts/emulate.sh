#!/usr/bin/env bash
# Dumb end-to-end smoke test: brings up the kernel + echo-plugin demo stack,
# waits for the plugin to register with the kernel over UDS, reports
# pass/fail, and tears the stack down.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

cleanup() {
  local code=$?
  if [ "$code" -ne 0 ]; then
    echo "--- kernel logs ---"
    docker compose logs kernel
    echo "--- plugin logs ---"
    docker compose logs plugin
  fi
  docker compose down -v
  exit "$code"
}
trap cleanup EXIT

docker compose up -d --build

echo "waiting for kernel /health..."
for _ in $(seq 1 30); do
  if curl -sf http://localhost:8080/health >/dev/null; then
    break
  fi
  sleep 1
done
curl -sf http://localhost:8080/health >/dev/null || { echo "FAIL: kernel never became healthy"; exit 1; }

echo "waiting for echo-plugin to register..."
for _ in $(seq 1 30); do
  if curl -sf http://localhost:8080/plugins | grep -q '"plugin_id":"echo-plugin"'; then
    echo "PASS: echo-plugin registered with kernel"
    exit 0
  fi
  sleep 1
done

echo "FAIL: echo-plugin never registered"
exit 1

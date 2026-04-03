#!/usr/bin/env bash
set -e

if ! command -v docker &>/dev/null; then
    echo "ERROR: docker is not installed — https://docs.docker.com/get-docker/"
    exit 1
fi

COMPOSE="docker compose -f docker/grafana/docker-compose.yml"
TOKEN_FILE="docker/grafana/tokens/grafana_api_key"

# Navigate to repo root regardless of where the script is called from
cd "$(git rev-parse --show-toplevel)"

echo "==> Starting Docker stack..."
$COMPOSE up -d

echo "==> Waiting for grafana-init to write API key..."
for i in $(seq 1 30); do
    if [ -s "$TOKEN_FILE" ]; then
        break
    fi
    sleep 1
done

if [ ! -s "$TOKEN_FILE" ]; then
    echo "ERROR: $TOKEN_FILE not found — check: $COMPOSE logs grafana-init"
    exit 1
fi

trap 'echo ""; echo "==> Stopping Docker stack..."; $COMPOSE down' EXIT

echo "==> Stack ready."
echo "    Grafana:    http://localhost:3000"
echo "    Prometheus: http://localhost:9090"
echo ""
echo "==> Running example (Ctrl+C to stop)..."
RUST_LOG=info GRAFANA_API_KEY=$(cat "$TOKEN_FILE") \
    cargo run --example grafana_metrics --features grafana

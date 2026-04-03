#!/usr/bin/env bash
set -e

if ! command -v docker &>/dev/null; then
    echo "ERROR: docker is not installed — https://docs.docker.com/get-docker/"
    exit 1
fi

ROOT="$(git rev-parse --show-toplevel)"
COMPOSE="docker compose -f $ROOT/docker/grafana/docker-compose.yml"
TOKEN_FILE="$ROOT/docker/grafana/tokens/grafana_api_key"

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
    cargo run --manifest-path "$ROOT/Cargo.toml" --example grafana_metrics --features grafana

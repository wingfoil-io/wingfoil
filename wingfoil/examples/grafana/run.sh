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

EXPLORE_URL='http://localhost:3000/explore?orgId=1&refresh=5s&left=%7B%22datasource%22%3A%22Prometheus%22%2C%22queries%22%3A%5B%7B%22refId%22%3A%22A%22%2C%22expr%22%3A%22wingfoil_ticks_total%22%7D%5D%2C%22range%22%3A%7B%22from%22%3A%22now-1m%22%2C%22to%22%3A%22now%22%7D%7D'

echo "==> Stack ready."
echo "    Grafana (metric, 5s auto-refresh): $EXPLORE_URL"
echo "    Prometheus: http://localhost:9090"
echo ""
echo "==> Running example (Ctrl+C to stop)..."
RUST_LOG=info GRAFANA_API_KEY=$(cat "$TOKEN_FILE") \
    cargo run --manifest-path "$ROOT/Cargo.toml" --example grafana_metrics --features grafana

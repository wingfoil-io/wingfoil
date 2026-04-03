#!/usr/bin/env bash
set -e

if ! command -v docker &>/dev/null; then
    echo "ERROR: docker is not installed — https://docs.docker.com/get-docker/"
    exit 1
fi

ROOT="$(git rev-parse --show-toplevel)"
COMPOSE="docker compose -f $ROOT/docker/grafana/docker-compose.yml"

echo "==> Starting Docker stack..."
$COMPOSE up -d

trap 'echo ""; echo "==> Stopping Docker stack..."; $COMPOSE down' EXIT

EXPLORE_URL='http://localhost:3000/explore?orgId=1&refresh=1s&left=%7B%22datasource%22%3A%22Prometheus%22%2C%22queries%22%3A%5B%7B%22refId%22%3A%22A%22%2C%22expr%22%3A%22rate%28wingfoil_ticks_total%5B1s%5D%29%22%7D%5D%2C%22range%22%3A%7B%22from%22%3A%22now-1m%22%2C%22to%22%3A%22now%22%7D%7D'

link() { printf '\e]8;;%s\e\\%s\e]8;;\e\\\n' "$1" "$2"; }

echo "==> Stack ready."
printf "    Grafana (metric, 5s auto-refresh): "
link "$EXPLORE_URL" "$EXPLORE_URL"
printf "    Prometheus: "
link "http://localhost:9090" "http://localhost:9090"
echo ""
echo "    For OTLP push support, run: cargo run --example otlp_metrics --features otlp,grafana"
echo ""
echo "==> Running example (Ctrl+C to stop)..."
RUST_LOG=info \
    cargo run --manifest-path "$ROOT/Cargo.toml" --example grafana_metrics --features grafana

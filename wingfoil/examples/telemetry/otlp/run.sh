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

EXPLORE_URL='http://localhost:3000/explore?orgId=1&refresh=1s&left=%7B%22datasource%22%3A%22Prometheus%22%2C%22queries%22%3A%5B%7B%22refId%22%3A%22A%22%2C%22expr%22%3A%22rate%28wingfoil_ticks_total%5B1s%5D%29%22%7D%5D%2C%22range%22%3A%7B%22from%22%3A%22now-1m%22%2C%22to%22%3A%22now%22%7D%7D'

link() { printf '\e]8;;%s\e\\%s\e]8;;\e\\\n' "$1" "$2"; }

echo "==> Stack ready."
printf "    Grafana (metric, 1s auto-refresh): "
link "$EXPLORE_URL" "$EXPLORE_URL"
printf "    Prometheus: "
link "http://localhost:9090" "http://localhost:9090"
echo ""
echo "==> Running example (Ctrl+C to stop)..."
echo "    NOTE: metrics are visible in Grafana via Prometheus scraping port 9091."
echo "          OTLP push to port 4318 requires a separate OTel collector."
echo ""
RUST_LOG=info OTLP_ENDPOINT=http://localhost:4318 \
    cargo run --manifest-path "$ROOT/Cargo.toml" --example otlp --features otlp,prometheus

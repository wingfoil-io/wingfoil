#!/usr/bin/env bash
# Bring up the Grafana + Prometheus stack, then run one of the two telemetry
# examples against it.
#
#   ./run.sh              # prometheus (pull) — the default
#   ./run.sh prometheus
#   ./run.sh otlp         # prometheus + OTLP push
#
# A port of the legacy `legacy/wingfoil/examples/telemetry/{prometheus,otlp}/run.sh`,
# collapsed into one script since the two differed only in which example they
# launched.
set -euo pipefail

EXAMPLE="${1:-prometheus}"
case "$EXAMPLE" in
prometheus)
    TARGET=prometheus_adapter
    FEATURES=prometheus
    ;;
otlp)
    TARGET=otlp_adapter
    FEATURES=otlp,prometheus
    ;;
*)
    echo "ERROR: unknown example \"$EXAMPLE\" — expected prometheus or otlp" >&2
    exit 1
    ;;
esac

if ! command -v docker &>/dev/null; then
    echo "ERROR: docker is not installed — https://docs.docker.com/get-docker/" >&2
    exit 1
fi

ROOT="$(git rev-parse --show-toplevel)"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPOSE="docker compose -f $HERE/docker/docker-compose.yml"

echo "==> Starting Docker stack..."
$COMPOSE up -d

trap 'echo ""; echo "==> Stopping Docker stack..."; $COMPOSE down' EXIT

EXPLORE_URL='http://localhost:3000/explore?orgId=1&refresh=1s&left=%7B%22datasource%22%3A%22Prometheus%22%2C%22queries%22%3A%5B%7B%22refId%22%3A%22A%22%2C%22expr%22%3A%22rate%28wingfoil_ticks_total%5B1s%5D%29%22%7D%5D%2C%22range%22%3A%7B%22from%22%3A%22now-1m%22%2C%22to%22%3A%22now%22%7D%7D'

link() { printf '\e]8;;%s\e\\%s\e]8;;\e\\\n' "$1" "$2"; }

echo "==> Stack ready."
printf "    Grafana (metric, 1s auto-refresh): "
link "$EXPLORE_URL" "$EXPLORE_URL"
printf "    Prometheus: "
link "http://localhost:9090" "http://localhost:9090"
echo ""

if [ "$EXAMPLE" = otlp ]; then
    echo "    NOTE: metrics reach Grafana via Prometheus scraping port 9091."
    echo "          The OTLP push to port 4318 needs a separate OTel collector:"
    echo "            docker run --rm -p 4318:4318 otel/opentelemetry-collector:0.149.0"
    echo ""
fi

echo "==> Running example (Ctrl+C to stop)..."
RUST_LOG=info \
    cargo run --manifest-path "$ROOT/Cargo.toml" -p wingfoil \
    --example "$TARGET" --features "$FEATURES"

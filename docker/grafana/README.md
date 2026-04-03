# Grafana Integration Test Environment

Docker Compose stack for running wingfoil Grafana adapter integration tests.

## Services

| Service    | Port | Purpose |
|------------|------|---------|
| Grafana    | 3000 | Dashboard UI + Live push target (no login required) |
| Prometheus | 9090 | Scrapes wingfoil's `/metrics` endpoint on port 9091 |

## Usage

```bash
docker compose -f docker/grafana/docker-compose.yml up -d   # start in background
docker compose -f docker/grafana/docker-compose.yml ps      # wait for all services healthy (~10s)
# Run integration tests (API key is read automatically from docker/grafana/tokens/grafana_api_key)
RUST_LOG=INFO cargo test --features grafana-integration-test -p wingfoil -- --test-threads=1 --nocapture
# when done:
# docker compose -f docker/grafana/docker-compose.yml down
```

## API Key

`docker compose up` automatically creates a service account and writes the token to
`docker/grafana/tokens/grafana_api_key` via the `grafana-init` container.

Integration tests read this file automatically — no manual steps needed.

If you need the key explicitly:
```bash
cat docker/grafana/tokens/grafana_api_key
```

## Environment Variables

| Variable                | Default                   | Description                        |
|-------------------------|---------------------------|------------------------------------|
| `GRAFANA_TEST_URL`      | `http://localhost:3000`   | Grafana base URL                   |
| `GRAFANA_TEST_API_KEY`  | —                         | Service account token (required)   |
| `GRAFANA_TEST_ORG_ID`   | `1`                       | Grafana org ID                     |
| `PROMETHEUS_TEST_URL`   | `http://localhost:9090`   | Prometheus base URL                |
| `WINGFOIL_METRICS_PORT` | `9091`                    | Port wingfoil binds for `/metrics` |

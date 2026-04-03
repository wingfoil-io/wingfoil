# Grafana Integration Test Environment

Docker Compose stack for running wingfoil Grafana adapter integration tests.

## Services

| Service    | Port | Purpose |
|------------|------|---------|
| Grafana    | 3000 | Dashboard UI + Live push target (admin/admin) |
| Prometheus | 9090 | Scrapes wingfoil's `/metrics` endpoint on port 9091 |

## Usage

```bash
# Start the stack
docker compose up

# Wait for healthy (grafana takes ~10s)
docker compose ps

# Run integration tests
GRAFANA_TEST_URL=http://localhost:3000 \
GRAFANA_TEST_API_KEY=<see below> \
PROMETHEUS_TEST_URL=http://localhost:9090 \
RUST_LOG=INFO cargo test --features grafana-integration-test -p wingfoil -- --test-threads=1 --nocapture

# Tear down
docker compose down
```

## API Key

`docker compose up` automatically creates a service account and writes the token to
`docker/grafana/tokens/grafana_api_key` via the `grafana-init` container.

Integration tests read this file automatically — no manual steps needed.

If you need the key explicitly:
```bash
cat tokens/grafana_api_key
```

## Environment Variables

| Variable                | Default                   | Description                        |
|-------------------------|---------------------------|------------------------------------|
| `GRAFANA_TEST_URL`      | `http://localhost:3000`   | Grafana base URL                   |
| `GRAFANA_TEST_API_KEY`  | —                         | Service account token (required)   |
| `GRAFANA_TEST_ORG_ID`   | `1`                       | Grafana org ID                     |
| `PROMETHEUS_TEST_URL`   | `http://localhost:9090`   | Prometheus base URL                |
| `WINGFOIL_METRICS_PORT` | `9091`                    | Port wingfoil binds for `/metrics` |

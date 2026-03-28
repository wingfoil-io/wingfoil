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
docker compose up -d

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

Grafana Live push requires an API key. Create a service account token after first start:

```bash
# Create service account
curl -X POST http://admin:admin@localhost:3000/api/serviceaccounts \
  -H 'Content-Type: application/json' \
  -d '{"name":"wingfoil-test","role":"Editor"}'

# Create token (use the id returned above)
curl -X POST http://admin:admin@localhost:3000/api/serviceaccounts/<id>/tokens \
  -H 'Content-Type: application/json' \
  -d '{"name":"wingfoil-test-token"}'
```

Copy the `key` field from the response into `GRAFANA_TEST_API_KEY`.

## Environment Variables

| Variable                | Default                   | Description                        |
|-------------------------|---------------------------|------------------------------------|
| `GRAFANA_TEST_URL`      | `http://localhost:3000`   | Grafana base URL                   |
| `GRAFANA_TEST_API_KEY`  | —                         | Service account token (required)   |
| `GRAFANA_TEST_ORG_ID`   | `1`                       | Grafana org ID                     |
| `PROMETHEUS_TEST_URL`   | `http://localhost:9090`   | Prometheus base URL                |
| `WINGFOIL_METRICS_PORT` | `9091`                    | Port wingfoil binds for `/metrics` |

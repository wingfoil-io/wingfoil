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
RUST_LOG=INFO cargo test --features prometheus-integration-test -p wingfoil -- --test-threads=1 --nocapture
# when done:
# docker compose -f docker/grafana/docker-compose.yml down
```

## Environment Variables

| Variable                | Default                   | Description                        |
|-------------------------|---------------------------|------------------------------------|
| `PROMETHEUS_TEST_URL`   | `http://localhost:9090`   | Prometheus base URL                |
| `WINGFOIL_METRICS_PORT` | `9091`                    | Port wingfoil binds for `/metrics` |

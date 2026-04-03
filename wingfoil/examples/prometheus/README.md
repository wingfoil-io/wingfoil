# Prometheus Adapter Example

Demonstrates the `PrometheusExporter` — serves `GET /metrics` on port 9091 so Grafana
can scrape it via its Prometheus data source.

## Setup

```sh
./wingfoil/examples/prometheus/run.sh
```

Starts the Docker stack (Grafana + Prometheus), then runs the example. Press `Ctrl+C` to stop.

- **Grafana** on <http://localhost:3000> (no login required)
- **Prometheus** on <http://localhost:9090>

To stop the stack:
```sh
docker compose -f docker/grafana/docker-compose.yml down
```

## Run

```sh
cargo run --example prometheus --features prometheus
```

## Code

```rust
let exporter = PrometheusExporter::new("0.0.0.0:9091");
let port = exporter.serve()?;

let counter = ticker(Duration::from_secs(1)).count();
let node = exporter.register("wingfoil_ticks_total", counter);

node.run(RunMode::RealTime, RunFor::Forever)?;
```

## Output

```
Prometheus metrics available at http://localhost:9091/metrics
```

Scraping `http://localhost:9091/metrics` returns:

```
# TYPE wingfoil_ticks_total gauge
wingfoil_ticks_total 5
```

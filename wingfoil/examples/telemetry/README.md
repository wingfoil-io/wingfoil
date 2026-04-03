# Telemetry Examples

Wingfoil provides two adapters for exporting metrics. Both work with Grafana as a
visualisation layer — they differ only in how data is transported.

## Which should I use?

| | `prometheus` | `otlp` |
|---|---|---|
| **Model** | Pull — Prometheus scrapes your app | Push — your app pushes to a collector |
| **Best for** | Existing Prometheus/Grafana stacks | Cloud-native / multi-vendor stacks |
| **Backends** | Prometheus, Mimir, Thanos | Grafana Alloy, Datadog, Honeycomb, New Relic, … |
| **Setup** | Zero config — just expose a port | Requires an OTel collector |
| **Standard** | De facto today | Emerging standard (growing fast) |

When in doubt, start with `prometheus` — it works with everything and needs no extra infrastructure.
Use `otlp` if you're pushing to a cloud backend or already running an OTel collector.

## Examples

| Example | Adapter | Run |
|---|---|---|
| [`prometheus/`](prometheus/) | `PrometheusExporter` | `./wingfoil/examples/telemetry/prometheus/run.sh` |
| [`otlp/`](otlp/) | `otlp_push` | `./wingfoil/examples/telemetry/otlp/run.sh` |

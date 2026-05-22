# LinkedIn — Telemetry

## Post

A short note on how Wingfoil handles telemetry, since it came up in a couple of conversations recently.

There are three pieces, and they're independent:

A Prometheus exporter that serves `/metrics` on a port you pick. You register a stream against a metric name and that's it — Grafana scrapes it like any other target.

An OTLP push adapter that takes a stream and ships values to anything that speaks OpenTelemetry. We use it for Grafana Alloy; other people have pointed it at Datadog and Honeycomb without issue.

Tracing spans for the graph engine itself. Behind a feature flag (`instrument-default` for the lifecycle bits, `instrument-all` if you want a span per node cycle, which gets noisy fast). No code changes — you flip the flag and the spans show up.

None of this is novel on its own. The thing we wanted was to not have to choose between Prometheus and OTLP at the library level, since different deployments tend to want different things. So we shipped both and let the graph decide.

## First comment

- Prometheus adapter: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/prometheus
- OTLP adapter: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/otlp
- Feature flags: https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/Cargo.toml

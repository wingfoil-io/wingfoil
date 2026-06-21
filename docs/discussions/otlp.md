<!--
POST TO:   https://github.com/open-telemetry/opentelemetry-rust/discussions/new?category=show-and-tell
REPO:      open-telemetry/opentelemetry-rust
CATEGORY:  Show and tell  (use General if Show and tell isn't available)
TITLE:     Wingfoil — a Rust stream-processing graph framework with an OTLP exporter
-->

---

Hi all,

Wanted to share something we recently shipped that builds on the OpenTelemetry
Rust SDK, in case it's useful to others here and to get any feedback on the
integration choices.

[Wingfoil](https://github.com/wingfoil-io/wingfoil) is a Rust stream-processing
library — you wire up a graph of nodes where each node ticks when its upstreams
produce a value, and the graph runs either in real time or replayed from
historical data. It's aimed at high-frequency trading and real-time AI pipelines.

We just added an OTLP adapter built on `opentelemetry`, `opentelemetry_sdk` and
`opentelemetry-otlp` (HTTP/protobuf), with two independent pathways:

- **Metrics** — `OtlpPush::otlp_push` exports stream values as OTel gauge
  metrics to any OTLP backend. Implemented for any `Stream<T: Display>`, so a
  numeric or string stream can be pushed without wrapping. A `SdkMeterProvider`
  with a 500 ms `PeriodicReader` is built per consumer so the final batch flushes
  on shutdown.
- **Traces** — `OtlpSpans::otlp_spans` emits one span per hop on a
  `Stream<P: HasLatency>`, attaching caller-supplied attributes (session id,
  request id, …) to the parent span. We reach for this for high-cardinality
  per-request data that would blow up Prometheus label cardinality.

Design choices we'd like eyes on from people who know the SDK well:

- The metric name has to be a `&'static str` to satisfy the `f64_gauge` builder —
  fine for us but worth flagging for anyone generating metric names dynamically.
- In historical/backtesting mode the consumer drains its source stream **without
  building a provider or making any network calls**, so replays don't emit
  telemetry. Curious whether there's a more idiomatic "null exporter" pattern for
  this than skipping provider construction entirely.

A concrete use case: pushing live latency/throughput gauges from a trading graph
to a collector for dashboards, while emitting per-request spans for the slow tail
so we can trace an individual request across graph hops.

Links:
- Adapter source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/otlp
- Wingfoil repo: https://github.com/wingfoil-io/wingfoil
- Project site: https://www.wingfoil.io/

Thanks for building the OpenTelemetry Rust SDK — happy to answer questions or dig
into anything above.

We're actively looking for contributors, so if any of this is up your street
we'd love the help. And if wingfoil looks useful to you, a ⭐ on the
[repo](https://github.com/wingfoil-io/wingfoil) would mean a lot.

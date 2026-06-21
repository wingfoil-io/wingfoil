<!--
POST TO:   https://github.com/prometheus/prometheus/discussions/new?category=show-and-tell
REPO:      prometheus/prometheus
CATEGORY:  Show and tell
TITLE:     Wingfoil — a Rust stream-processing graph framework with a Prometheus exporter
-->

---

Hi all,

Wanted to share something we recently shipped that builds on Prometheus, in case
it's useful to others here and to get any feedback on the integration choices.

[Wingfoil](https://github.com/wingfoil-io/wingfoil) is a Rust stream-processing
library — you wire up a graph of nodes where each node ticks when its upstreams
produce a value, and the graph runs either in real time or replayed from
historical data. It's aimed at high-frequency trading and real-time AI pipelines.

We just added a Prometheus exporter that turns any stream value into a scrapable
metric: attach a metric node to a stream, and the exporter serves
`GET /metrics` in the Prometheus text format for Prometheus/Grafana to scrape.

Because this sits on a latency-critical graph thread, the hot-path design was the
interesting part and is where we'd love feedback:

- Each metric node reads `peek_value()` on tick and publishes the stringified
  value into a per-metric `Arc<ArcSwap<String>>` via a lock-free atomic pointer
  swap — **no locking from the graph `cycle()`**.
- The HTTP server runs on its own thread. On scrape it briefly locks a registry
  (`Mutex<Vec<(name, slot)>>`), snapshots it, drops the lock, then `.load()`s
  each slot to render — so the only lock contention is off the graph thread.
- In historical/backtesting run mode `cycle()` becomes a no-op and no metrics are
  written, so the same graph can be replayed offline without emitting noise.

We deliberately hand-roll the text format rather than pull in a metrics crate,
since the exposition format is simple and we wanted zero added deps on the hot
path — interested whether that's a reasonable call for a pull-based exporter or
whether we're missing edge cases (escaping, `# HELP`/`# TYPE`, etc.).

Links:
- Adapter source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/prometheus
- Wingfoil repo: https://github.com/wingfoil-io/wingfoil
- Project site: https://www.wingfoil.io/

Thanks for building Prometheus — happy to answer questions or dig into anything
above.

We're actively looking for contributors, so if any of this is up your street
we'd love the help. And if wingfoil looks useful to you, a ⭐ on the
[repo](https://github.com/wingfoil-io/wingfoil) would mean a lot.

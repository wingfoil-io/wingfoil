<!--
POST TO:   https://github.com/infinyon/fluvio/discussions/new?category=show-and-tell
REPO:      infinyon/fluvio
CATEGORY:  Show and tell  (use General if Show and tell isn't available)
TITLE:     Wingfoil — a Rust stream-processing graph framework with a Fluvio adapter
-->

---

Hi all,

Wanted to share something we recently shipped that builds on Fluvio, in case it's
useful to others here and to get any feedback on the integration choices.

[Wingfoil](https://github.com/wingfoil-io/wingfoil) is a Rust stream-processing
library — you wire up a graph of nodes where each node ticks when its upstreams
produce a value, and the graph runs either in real time or replayed from
historical data. It's aimed at high-frequency trading and real-time AI pipelines.

We just added a Fluvio adapter exposing topics as wingfoil source and sink nodes:

- `fluvio_sub(conn, topic, partition, start_offset)` streams records from a
  partition into the graph as `Burst<FluvioEvent>`. `start_offset: None` reads
  from the beginning; `Some(n)` does an absolute seek.
- `fluvio_pub(conn, topic, upstream)` writes `FluvioRecord`s (keyed or keyless),
  flushing **once per burst** so a graph tick that produces several records turns
  into one batched produce.

Being a Rust-native streaming platform, Fluvio was a particularly natural fit —
the whole adapter is async end-to-end with no FFI. A concrete use case: a
wingfoil graph consuming a Fluvio topic of raw events, normalising and enriching
them, and producing the derived stream back to another topic — with the same
graph runnable in historical mode against a captured offset range for
backtesting.

One thing we'd appreciate guidance on from folks here: the cleanest
container/topology for self-contained integration tests (we currently spin an
`infinyon/fluvio` SC in `testcontainers` and create the topic up front, but SPU
registration timing has been the fiddly part).

Links:
- Adapter source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/fluvio
- Wingfoil repo: https://github.com/wingfoil-io/wingfoil
- Project site: https://www.wingfoil.io/

Thanks for building Fluvio — happy to answer questions or dig into anything above.

We're actively looking for contributors, so if any of this is up your street
we'd love the help. And if wingfoil looks useful to you, a ⭐ on the
[repo](https://github.com/wingfoil-io/wingfoil) would mean a lot.

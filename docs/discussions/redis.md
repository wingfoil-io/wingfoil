<!--
POST TO:   https://github.com/redis/redis/discussions/new?category=show-and-tell
REPO:      redis/redis
CATEGORY:  Show and tell
TITLE:     Wingfoil — a Rust stream-processing graph framework with a Redis adapter
-->

---

Hi all,

Wanted to share something we recently shipped that builds on Redis, in case it's
useful to others here and to get any feedback on the integration choices.

[Wingfoil](https://github.com/wingfoil-io/wingfoil) is a Rust stream-processing
library — you wire up a graph of nodes where each node ticks when its upstreams
produce a value, and the graph runs either in real time or replayed from
historical data. It's aimed at high-frequency trading and real-time AI pipelines.

We just added a Redis adapter that exposes Redis as wingfoil source and sink
nodes, covering **two transports**:

- **Pub/Sub** — `redis_sub` subscribes to a channel and emits each message into
  the graph; `redis_pub` publishes a stream of messages back out.
- **Streams** — `redis_stream_read` does a snapshot-then-tail read of a stream,
  and `redis_stream_write` appends via `XADD`.

A couple of design points we'd love eyes on:

- **Snapshot → tail handoff.** For streams we run `XRANGE key - +` to replay the
  backlog, capture the last entry ID, then `XREAD BLOCK 0 STREAMS key <last_id>`
  for the live tail. Because the tail reads strictly *after* the snapshot
  boundary ID, anything appended in the handoff window is delivered by `XREAD`
  (its ID is greater) and nothing is re-delivered. No missed or duplicated
  entries.
- **Pub/Sub is fire-and-forget.** No backlog/replay/offsets, so a subscriber
  only sees messages published after its `SUBSCRIBE` completes — we treat that as
  an explicit no-snapshot path rather than papering over it.
- Built on async connections (`tokio-comp`); the blocking `XREAD` tail runs on a
  dedicated connection so blocking it indefinitely is safe.

A concrete use case: fanning out a normalised market-data stream over Redis
Pub/Sub to a fleet of lightweight consumers, while persisting the same events to
a Redis Stream for replay/backtesting through wingfoil's historical run mode.

Links:
- Adapter source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/redis
- Wingfoil repo: https://github.com/wingfoil-io/wingfoil
- Project site: https://www.wingfoil.io/

Thanks for building Redis — happy to answer questions or dig into anything above.

We're actively looking for contributors, so if any of this is up your street
we'd love the help. And if wingfoil looks useful to you, a ⭐ on the
[repo](https://github.com/wingfoil-io/wingfoil) would mean a lot.

# GitHub discussion — eclipse-iceoryx/iceoryx2

Suggested category: **Show and tell** (or **General**)

Suggested title: **Wingfoil — a Rust stream-processing graph with an iceoryx2 adapter**

---

## Body

Hi all,

Wanted to share something we recently shipped that builds on iceoryx2, in case it's useful to others here and to get any feedback you have on the integration choices.

### Context

Wingfoil is a Rust stream-processing library — you build a DAG of nodes (map, filter, fold, custom) where each node ticks when its upstreams produce a value, and the graph runs either in real time or replayed from historical data. We use it for high-frequency trading data pipelines.

Up to now, splitting a graph across processes meant TCP / Unix sockets — fine for most things, but for in-box hot paths (e.g. order gateway → risk check → execution) the kernel round-trip starts to show. So we wrote an iceoryx2 adapter.

### What the adapter looks like

Two graph nodes:

```rust
use wingfoil::adapters::iceoryx2::{iceoryx2_pub, iceoryx2_sub};
use wingfoil::*;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, ZeroCopySend)]
struct Quote { price: u64, qty: u32 }

// Subscriber side — pulls from shared memory, emits into the graph
let quotes = iceoryx2_sub::<Quote>("market_quotes")
    .collapse()
    .for_each(|q, _| risk.update(q));

// Publisher side — graph values written into shared memory
mid_stream.iceoryx2_pub(opts);
```

Payload constraints inherit straight from iceoryx2: `#[repr(C)]` + `ZeroCopySend`. The compiler enforces it, which is most of the appeal.

### Three polling modes

The trickiest design call was how the subscriber pulls from the service, given wingfoil's graph engine runs synchronously per cycle. We landed on three modes, picked at construction time:

- **Spin** — direct `try_receive` inside `cycle()`. ~1–5 µs end-to-end, pins a core.
- **Threaded** — background thread with a 10 µs yield, channel into the graph. Lower CPU, ~10–100 µs.
- **Signaled** — event-driven `WaitSet` on a matching `Event` service; publisher signals after each send.

These cover the real situations we've hit: spin for the latency-critical path, signaled for the wake-up-when-something-happens case, threaded for the middle ground where the consumer is doing other work too.

### What we're using it for

Splitting an order gateway from a risk check that previously lived in the same binary. Same machine, two processes, sub-microsecond handoff. The `ZeroCopySend` constraint caught one struct-layout bug at compile time that would have been unpleasant at runtime.

### Things we'd appreciate feedback on

- **The polling-mode trichotomy.** Have other integrators landed on a different shape here? Signaled mode feels like it should be the default once you've got it working, but spin still wins on the fastest path.
- **Service-contract evolution.** We lean on iceoryx2's contract checks, but versioning a payload struct in a non-breaking way (adding a field, deprecating one) is something we're still iterating on. Curious how others handle this in long-running deployments.

### Links

- Adapter source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/iceoryx2
- Pub example: https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/iceoryx2/pub.rs
- Sub example: https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/iceoryx2/sub.rs
- Wingfoil: https://github.com/wingfoil-io/wingfoil

Thanks for building iceoryx2 — happy to answer questions or dig into anything above.

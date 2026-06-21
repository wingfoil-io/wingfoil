<!--
POST TO:   https://github.com/real-logic/aeron/discussions/new?category=show-and-tell
REPO:      real-logic/aeron  (aeron-io/aeron)
CATEGORY:  Show and tell  (use General if Show and tell isn't available)
TITLE:     Wingfoil — a Rust stream-processing graph framework with an Aeron adapter
-->

---

Hi all,

Wanted to share something we recently shipped that builds on Aeron, in case it's
useful to others here and to get any feedback on the integration choices.

[Wingfoil](https://github.com/wingfoil-io/wingfoil) is a Rust stream-processing
library — you wire up a graph of nodes where each node ticks when its upstreams
produce a value, and the graph runs either in real time or replayed from
historical data. It's aimed at high-frequency trading and real-time AI pipelines.

We just added an Aeron adapter that wraps a subscription/publication as wingfoil
source and sink nodes:

- `aeron_sub_fragment` polls a subscription via the fragment-level
  `poll(FragmentHandler, limit)` surface and emits `Burst<T>`. The parser runs
  per fragment and gets the `FragmentHeader` (position, session id, stream id);
  a parser error drops that fragment without stopping the graph.
- `AeronPub::aeron_pub` offers serialised values to a channel, with
  `_with_status` variants that surface connect/disconnect/back-pressure
  transitions as a reactive side-channel stream.

**Up front, since this group will spot it immediately:** the subscriber works at
the *fragment* level and does **not** currently wrap a `FragmentAssembler`, so
the parser sees one fragment per callback rather than a reassembled message. In
practice that means it assumes messages fit within a single fragment (sub-MTU)
today. Adding optional fragment reassembly (and likely a `controlledPoll`
variant for flow control) is the obvious next step — feedback on whether we
should reassemble by default, or keep the raw-fragment surface as the primary
one and layer assembly on top, is exactly the kind of input we're after.

The design points we'd most like feedback on:

- **Two polling modes.** `Spin` polls Aeron *inside* the graph `cycle()` on the
  graph thread — zero thread-crossing latency, burns a core, ticks downstream
  only when fragments actually arrive. `Threaded` polls on a background thread
  and delivers over a channel (one hop of latency, frees the graph thread).
- **Two backends.** A `rusteron-client` C/C++ FFI backend for production
  (genuinely lock-free `poll()`/`offer()`), and an experimental pure-Rust
  `aeron-rs` backend. The latter shares `Arc<Mutex<…>>` handles with its own
  client-conductor thread, so the lock can't be hoisted out of `cycle()` — we
  detect that and automatically downgrade `Spin` to `Threaded` for it rather
  than violate our "no locks in `cycle()`" invariant. Curious whether that
  matches how others have integrated aeron-rs.

A concrete use case: a `Spin`-mode subscriber feeding a wingfoil graph that
does order-book construction and risk checks, publishing decisions back out over
`aeron:ipc` to a co-located execution process — all on one core with no kernel
round-trips.

Links:
- Adapter source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/aeron
- Wingfoil repo: https://github.com/wingfoil-io/wingfoil
- Project site: https://www.wingfoil.io/

Thanks for building Aeron — happy to answer questions or dig into anything above.

We're actively looking for contributors, so if any of this is up your street
we'd love the help. And if wingfoil looks useful to you, a ⭐ on the
[repo](https://github.com/wingfoil-io/wingfoil) would mean a lot.

<!--
POST TO:   https://github.com/etcd-io/etcd/discussions/new?category=show-and-tell
REPO:      etcd-io/etcd
CATEGORY:  Show and tell
TITLE:     Wingfoil — a Rust stream-processing graph framework with an etcd adapter
-->

---

Hi all,

Wanted to share something we recently shipped that builds on etcd, in case it's
useful to others here and to get any feedback on the integration choices.

[Wingfoil](https://github.com/wingfoil-io/wingfoil) is a Rust stream-processing
library — you wire up a graph of nodes where each node ticks when its upstreams
produce a value, and the graph runs either in real time or replayed from
historical data. It's aimed at high-frequency trading and real-time AI pipelines.

We just added an etcd adapter exposing a key-prefix as wingfoil source and sink
nodes:

- `etcd_sub(conn, prefix)` emits a **snapshot** of all current keys under the
  prefix, then streams **live watch** events (Put and Delete) as `Burst<EtcdEvent>`.
- `etcd_pub(conn, upstream, lease_ttl)` issues one PUT per entry. With
  `lease_ttl: Some(..)` it attaches a lease with automatic keepalive renewal and
  revokes it on clean shutdown, so keys vanish immediately when the node stops —
  handy for service-presence / liveness keys.

The bit we'd most like feedback on is the **snapshot → watch handoff**: we open
the `WATCH(prefix)` *before* the `GET(prefix)`, then filter watch events with
`mod_revision ≤ snapshot_rev`. Any write committed in the handoff window lands in
the watch stream with a higher revision (never missed), and anything already in
the GET is filtered as a duplicate. It mirrors the pattern we'd expect, but it'd
be great to hear if there's a sharper idiom we've missed.

A concrete use case: distributing live config / strategy parameters to a fleet of
wingfoil trading graphs — each graph watches a prefix and reacts to parameter
changes as ordinary stream ticks, with leased keys advertising which graphs are
currently up.

Links:
- Adapter source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/etcd
- Wingfoil repo: https://github.com/wingfoil-io/wingfoil
- Project site: https://www.wingfoil.io/

Thanks for building etcd — happy to answer questions or dig into anything above.

We're actively looking for contributors, so if any of this is up your street
we'd love the help. And if wingfoil looks useful to you, a ⭐ on the
[repo](https://github.com/wingfoil-io/wingfoil) would mean a lot.

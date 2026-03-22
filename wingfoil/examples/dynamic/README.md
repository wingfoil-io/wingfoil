
## Dynamic Graphs

- [View examples](https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/dynamic/)
- Add and remove nodes at runtime without stopping execution
- Three approaches: high-level `dynamic_group_stream`, hand-rolled `MutableNode`, and static `demux_it`

Wingfoil supports modifying the graph at runtime — adding and removing nodes
between engine cycles — without stopping execution.

Three examples cover this from different angles:

- **[`dynamic-group`](dynamic-group/main.rs)** — high-level API using
  [`dynamic_group_stream`] to wire per-instrument subgraphs on demand.
- **[`dynamic-manual`](dynamic-manual/main.rs)** — low-level equivalent:
  a custom [`MutableNode`] that calls `state.add_upstream()` and
  `state.remove_node()` directly.
- **[`demux`](demux/main.rs)** — statically-wired alternative using
  [`demux_it`] with a fixed-capacity slot pool; no dynamic wiring required.

All three build the same price aggregator: instruments are created and deleted
at runtime, and a running price book is maintained across the changes.

```bash
cargo run --example dynamic-group --features dynamic-graph-beta
cargo run --example dynamic-manual --features dynamic-graph-beta
cargo run --example demux
```

## Observability — the `logged` debug tap and the engine's spans

Three modes, mirroring the legacy wingfoil `tracing` example.

### `log` — the `logged` debug tap

`logged(label, level)` taps a stream: it emits each value as it ticks through
the `log` crate (`"{time} {label} {value:?}"`, target `"wingfoil"`) and passes
the value through unchanged. Point any `log`-compatible subscriber at it to see
the graph's activity without altering the data flow.

```sh
RUST_LOG=info cargo run -p wingfoil-next --example tracing
```

```text
[.. INFO  wingfoil] 0.000_000 tick 1
[.. INFO  wingfoil] 1.000_000 tick 2
[.. INFO  wingfoil] 2.000_000 tick 3
```

### `tracing` — the same events through a `tracing` subscriber

```sh
RUST_LOG=info cargo run -p wingfoil-next --example tracing --features tracing -- tracing
```

```text
2026-01-01T00:00:00.000000Z  INFO wingfoil: 0.000_000 tick 1
2026-01-01T00:00:00.000000Z  INFO wingfoil: 1.000_000 tick 2
2026-01-01T00:00:00.000000Z  INFO wingfoil: 2.000_000 tick 3
```

Next's `logged` emits through the `log` crate rather than the `tracing` event
macros (legacy routes it through `tracing` because it takes `tracing` as an
unconditional dependency; next's is optional). The records still reach the
subscriber — `tracing_subscriber`'s `init()` installs the `tracing-log` bridge —
and, as the `instruments` mode below shows, they arrive *inside* the engine's
span context, exactly as legacy's do.

### `instruments` — the engine's own spans

Adds span open/close events, so the engine's `instrument-*` instrumentation
becomes visible on top of the events above:

```sh
RUST_LOG=info cargo run -p wingfoil-next --example tracing \
    --features instrument-default -- instruments
```

```text
INFO initialise: wingfoil_next::interp: enter
INFO initialise: wingfoil_next::interp: close time.busy=127µs time.idle=14.5µs
INFO run: wingfoil_next::interp: enter
INFO run:apply_nodes{desc="start"}: wingfoil_next::interp: enter
INFO run:apply_nodes{desc="start"}: wingfoil_next::interp: close ...
INFO run:cycle: wingfoil_next::interp: enter
INFO run:cycle: wingfoil: 0.000_000 tick 1
INFO run:cycle: wingfoil_next::interp: close ...
...
INFO run:apply_nodes{desc="teardown"}: wingfoil_next::interp: close ...
INFO run: wingfoil_next::interp: close time.busy=1.94ms time.idle=6.75µs
```

`instrument-default` covers `run`, `initialise`, `apply_nodes` and `cycle`. Add
the per-node spans with `--features instrument-all`:

```text
INFO run:cycle:cycle_node{index=0 node="Ticker"}: wingfoil_next::interp: enter
INFO run:cycle:cycle_node{index=1 node="Count"}:  wingfoil_next::interp: enter
INFO run:cycle:cycle_node{index=2 node="Logged"}: wingfoil: 0.000_000 tick 1
```

See the crate docs ("Tracing and instrumentation") for the full feature table.

### Parity note

All three legacy modes are ported. Two deviations, both benign and both
visible above: next's `logged` reaches the subscriber through the `tracing-log`
bridge rather than the `tracing` event macros, and next has no separate `setup`
lifecycle phase (ops are constructed at wiring time), so its `apply_nodes` spans
cover `start` / `stop` / `teardown` where legacy's cover four phases.

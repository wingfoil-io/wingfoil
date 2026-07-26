## Observability — the `logged` debug tap

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

### Parity note — this is a partial port of the classic `tracing` example

The classic wingfoil `tracing` example demonstrates three modes:

| mode | what it does | status in next |
|------|--------------|----------------|
| `log` | events via `env_logger` | ✅ ported (this example) |
| `tracing` | same events routed through a `tracing-subscriber` | ⏳ not yet — needs the `tracing` feature ported to next |
| `instruments` | tracing **spans** around `run` and each engine cycle | ⏳ not yet — needs the engine's `instrument-*` features ported to next |

Next's op catalog logs through the `log` crate, and the engine has no span
instrumentation yet, so only the `log` mode is faithful today. Passing
`tracing` or `instruments` prints a note and falls back to `log`. The remaining
two modes are tracked as a Phase-6 item in `docs/port-plan.md`; they land with
the next engine's tracing / instrumentation port, not as example work.

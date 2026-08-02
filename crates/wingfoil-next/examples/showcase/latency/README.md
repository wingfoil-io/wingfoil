# Latency Measurement Example (wingfoil-next)

A two-process pipeline over iceoryx2 that demonstrates per-hop latency
stamping with `latency_stages!` + `Traced<T, L>` + `.stamp::<Stage>()`, and
end-of-run reporting via `latency_report`.

A port of the legacy `legacy/wingfoil/examples/latency` onto the next engine. It is
also the cross-process acceptance test for the Phase-5 latency infrastructure:
the stamps written in one process are read back and differenced in another,
with `Traced<Quote, QuoteLatency>` riding shared memory as a plain `#[repr(C)]`
payload.

## What it shows

- A `QuoteLatency` record declared once with the `latency_stages!` macro,
  shared by both processes.
- A `Traced<Quote, QuoteLatency>` payload — `#[repr(C)]` and `ZeroCopySend`
  — flowing through iceoryx2 shared memory with no allocation.
- Stage stamping at each hop using `.stamp::<quote_latency::<stage>>()`,
  which writes the cycle-start wall clock into the named field. One `u64`
  store per stamp, no syscall.
- A `latency_report` sink that aggregates per-stage delta statistics
  (count, min, mean, p50, p99, max) and prints the report on shutdown.

## Pipeline shape

```
publisher process                  | subscriber process
                                   |
ticker → produce → publish ───iceoryx2───→ receive → strategy → ack → report
                                   |
   ↑                  ↑            |       ↑          ↑          ↑
   stamp              stamp        |       stamp      stamp      stamp
```

Each `→ stamp` is a single `u64` write into the embedded `QuoteLatency`
record. The deltas the report prints are:

| pair | what it measures |
|---|---|
| `produce → publish` | publisher in-process serialization & enqueue |
| `publish → receive` | iceoryx2 IPC hop (shared memory + signal) |
| `receive → strategy` | subscriber strategy logic |
| `strategy → ack` | subscriber response construction |

## Running

Start the subscriber first so it doesn't miss messages, then the publisher:

```bash
# Terminal 1: subscriber, bounded to 10s so the report prints on exit
cargo run -p wingfoil-next --example latency_sub --features iceoryx2 -- 10

# Terminal 2: publisher
cargo run -p wingfoil-next --example latency_pub --features iceoryx2
```

The subscriber prints a report like:

```
latency report (delta from previous stage, nanoseconds):
  stage                         count          min         mean          p50          p99          max
  produce -> publish               40            0            0            2            2            0
  publish -> receive               40        75075       119875       131072       262144       164493
  receive -> strategy              40            0            0            2            2            0
  strategy -> ack                  40            0            0            2            2            0
```

The three in-process deltas are **zero by construction**: `.stamp()` reads a
per-cycle wall-clock snap, and those stages all run in the same engine cycle,
so they share a timestamp. Only the IPC hop crosses a cycle boundary and shows
a real number. Swap the in-process stamps for `.stamp_precise::<..>()` to get
intra-cycle resolution — see **Time source** below.

## Time source

Stamps always read wall-clock time (never engine time, which is source-driven
in historical mode). Two variants:

- `.stamp::<X>()` reads `Ctx::wall_time()` — a cycle-start snap, one `u64`
  load. Stages that tick in the same engine cycle share the timestamp, so
  deltas between intra-cycle stages are zero. Use this for coarse
  cross-process / cross-cycle measurement.
- `.stamp_precise::<X>()` reads `Ctx::wall_time_precise()` — a fresh TSC
  read (~5-10 ns). Gives intra-cycle resolution so in-process stages get
  distinct timestamps.

This pipeline works identically in realtime and historical mode — the same
wiring on a backtest gives you per-stage replay performance, and in production
gives you per-stage latency.

## Toggling

Each method has an `_if(enabled: bool)` variant that returns the upstream
unchanged when `enabled == false` — no node inserted into the graph, zero
runtime cost. Thread a single config flag through your pipeline builder:

```rust
let stamp = cfg.instrument_latency;
let pipe = incoming
    .stamp_if::<quote_latency::receive>(stamp)
    .map(strategy)
    .stamp_if::<quote_latency::strategy>(stamp)
    .stamp_if::<quote_latency::ack>(stamp);
let (sink, _) = pipe.latency_report_if(stamp, /* print */ true);
```

## Caveats

- Both processes must declare `latency_stages! { QuoteLatency { ... } }`
  in the same order. The `shared.rs` file in this example does it once
  and is `#[path]`-included by both binaries.
- Both processes must run on the same machine for the timestamps to be
  comparable (iceoryx2 is shared-memory-only, so this is the case here).
  For host-to-host timing you would need PTP-level clock sync.
- A crashed or `SIGKILL`ed run can leave a stale iceoryx2 service behind, and
  the next run then fails at start with `IncompatibleTypes` or a config
  mismatch. Clear it with `rm -rf /tmp/iceoryx2 /dev/shm/iox2_*`.

## Deviations from the legacy example

Both are fixes for defects in the legacy pair, not changes to what the
example teaches. Neither affects the library surface, so neither is a
[deviation-register](../../../../../docs/deviation-register.md) entry.

1. **`#[type_name(...)]` on both payload types.** The default
   `ZeroCopySend::type_name()` is `core::any::type_name::<Self>()`, which
   embeds the absolute Rust path — and since `shared.rs` is `#[path]`-included
   by two binary crates, that is `latency_pub::shared::Quote` in one process
   and `latency_sub::shared::Quote` in the other. iceoryx2 compares those
   strings when opening the service and rejects the second process with
   `IncompatibleTypes`. Legacy documents this hazard on its `Traced<T, L>`
   `ZeroCopySend` impl and ships `#[type_name(...)]` to escape it, but never
   applies it in `examples/latency` — so the legacy pair aborts at publisher
   start. Pinning both names fixes it.
2. **The subscriber takes an optional run duration.** Legacy runs
   `RunFor::Forever` and its README says to stop with Ctrl-C to get the
   report — but `print_on_teardown` fires from graph teardown, which a
   `SIGINT` never reaches, so on that path the report is never printed.
   Passing a duration runs to a clean stop and emits the report; omitting it
   keeps legacy's run-forever behaviour.

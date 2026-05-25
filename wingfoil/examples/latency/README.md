# Latency Measurement Example

A two-process pipeline over iceoryx2 that demonstrates per-hop latency
stamping with `latency_stages!` + `Traced<T, L>` + `.stamp::<Stage>()`, and
end-of-run reporting via `LatencyReport`.

## What it shows

- A `QuoteLatency` record declared once with the `latency_stages!` macro,
  shared by both processes.
- A `Traced<Quote, QuoteLatency>` payload — `#[repr(C)]` and `ZeroCopySend`
  — flowing through iceoryx2 shared memory with no allocation.
- Stage stamping at each hop using `.stamp::<quote_latency::<stage>>()`,
  which writes `state.time()` (cycle-start engine time) into the named
  field. One `u64` store per stamp, no syscall.
- A `LatencyReport` sink that aggregates per-stage delta statistics
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

Start the subscriber first so it doesn't miss messages, then the
publisher:

```bash
# Terminal 1: subscriber
cargo run --example latency_sub --features iceoryx2-beta

# Terminal 2: publisher
cargo run --example latency_pub --features iceoryx2-beta
```

Stop both with Ctrl-C. The subscriber prints a report like:

```
latency report (delta from previous stage, nanoseconds):
  stage                         count          min         mean          p50          p99          max
  produce -> publish              312          120          250          256         1024         3200
  publish -> receive              312         1850         3120         4096         8192        12000
  receive -> strategy             312           80          140          128          256          720
  strategy -> ack                 312           60          110          128          256          640
```

## Time source

Stamps always read wall-clock time (never engine time). Two variants:

- `.stamp::<X>()` reads `state.wall_time()` — a cycle-start snap, one `u64`
  load. Stages that tick in the same engine cycle share the timestamp, so
  deltas between intra-cycle stages are zero. Use this for coarse
  cross-process / cross-cycle measurement.
- `.stamp_precise::<X>()` reads `state.wall_time_precise()` — a fresh TSC
  read (~5-10 ns). Gives intra-cycle resolution so in-process stages get
  distinct timestamps.

This pipeline works identically in realtime and historical mode — the same
wiring on a backtest gives you per-stage replay performance, and in
production gives you per-stage latency.

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

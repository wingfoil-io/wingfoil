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

## Caveats

- Stamps default to `state.time()`, the cycle-start timestamp. Stages that
  fall in the same engine cycle will share the same timestamp (delta = 0).
  This is the intended trade-off — it makes stamping nearly free at the
  cost of intra-cycle resolution. Use `NanoTime::now()` directly if you
  need finer granularity at a specific hop.
- Both processes must declare `latency_stages! { QuoteLatency { ... } }`
  in the same order. The `shared.rs` file in this example does it once
  and is `#[path]`-included by both binaries.
- Both processes must run on the same machine for the timestamps to be
  comparable (iceoryx2 is shared-memory-only, so this is the case here).
  For host-to-host timing you would need PTP-level clock sync.

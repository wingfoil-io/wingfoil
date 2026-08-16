# Latency Measurement Example (wingfoil)

A two-process pipeline over iceoryx2 that demonstrates per-hop latency
stamping with `latency_stages!` + `Traced<T, L>` + `.stamp_as::<Stage>(mode)`,
and end-of-run reporting via `latency_report`.

A port of the legacy `legacy/wingfoil/examples/latency` onto the wingfoil engine. It is
also the cross-process acceptance test for the Phase-5 latency infrastructure:
the stamps written in one process are read back and differenced in another,
with `Traced<Quote, QuoteLatency>` riding shared memory as a plain `#[repr(C)]`
payload.

## What it shows

- A `QuoteLatency` record declared once with the `latency_stages!` macro,
  shared by both processes.
- A `Traced<Quote, QuoteLatency>` payload — `#[repr(C)]` and `ZeroCopySend`
  — flowing through iceoryx2 shared memory with no allocation.
- Stage stamping at each hop using `.stamp_as::<quote_latency::<stage>>(mode)`
  — `.stamp_each_as` on the subscriber, whose stream is burst-shaped — which
  writes the wall clock into the named field. One `u64` store per stamp, no
  syscall.
- `Stamping` as an *argument*, so which clock to read is one value threaded
  through the wiring rather than a choice baked into four method names.
- A `latency_report` sink that aggregates per-stage delta statistics
  (count, min, mean, p50, p99, p99.9, max) plus an end-to-end total, and
  prints the report on shutdown.
- A pipeline that stays **burst-shaped** end to end. The subscriber does
  *not* `collapse()` its `Burst` — collapse keeps only the burst's last
  value, and bursts only grow when the publisher outruns a graph cycle, so
  collapsing would drop exactly the samples taken while the system was busy
  and bias the histogram. See the `collapse` rustdoc for the full trap.

## Pipeline shape

```
publisher process                  | subscriber process
                                   |
ticker → produce → publish ───iceoryx2───→ receive → strategy → ack → report
                                   |
   ↑                  ↑            |       ↑          ↑          ↑
   stamp              stamp        |       stamp      stamp      stamp
```

The report adds one row the diagram does not: `produce → ack`, the end-to-end
total, measured on the message itself.

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
cargo run -p wingfoil --example latency_sub --features iceoryx2 -- 10

# Terminal 2: publisher
cargo run -p wingfoil --example latency_pub --features iceoryx2
```

The subscriber prints a report like:

```
latency report (delta from previous stage, nanoseconds):
  stage                            count          min         mean          p50          p99        p99.9          max
  produce -> publish                 100          180          465          426          968         1095         1095
  publish -> receive                 100        22475        60921        59221        90624        91138        91138
  receive -> strategy                100          322         1640         1648         2720         3738         3738
  strategy -> ack                    100          103          526          509          984         1040         1041
  produce -> ack (end to end)        100        25176        63553        62080        92672        93696        93841
```

Captured from a `--release` build, `latency_sub 12`, on a shared 4-core cloud
VM — so the IPC row is a reading of *that* machine under contention, not a
figure for the transport. `min` (22.5 µs) is the closest thing here to the hop
itself.

The last row is the **end-to-end total**, `produce → ack` measured on the
message rather than summed from the hops above it. Summing would be wrong in
general: a hop that skipped an observation (see below) is exactly the hop that
would make the sum disagree with reality.

`min`, `mean`, `max` and `count` are exact. `p50`, `p99` and `p99.9` are read
out of a sub-bucketed histogram and carry at most 3.125% relative error — and
are clamped to `[min, max]`, so a percentile is always a value the stage could
actually have observed.

### Rows without numbers

Both processes stamp with `Stamping::Precise`, which is why every row above
carries real figures. Switch `STAMPING` to `Stamping::Cycle` in `pub.rs` and
`sub.rs` and the same run prints this instead:

```
latency report (delta from previous stage, nanoseconds):
  stage                            count          min         mean          p50          p99        p99.9          max
  produce -> publish                   0            -            -            -            -            -            -  (100 same-cycle)
  publish -> receive                  65          737        24419        22272        80896        80896        81607  (35 backwards)
  receive -> strategy                  0            -            -            -            -            -            -  (100 same-cycle)
  strategy -> ack                      0            -            -            -            -            -            -  (100 same-cycle)
  produce -> ack (end to end)         65          737        24419        22272        80896        80896        81607  (35 backwards)
```

Every one of those notes is a real finding, and none of them was visible
before:

- **`same-cycle`** on the three in-process rows. `Stamping::Cycle` reads a
  per-cycle wall-clock snap, so stages running in the same engine cycle share a
  timestamp and the hop between them is not measured *at all*. This used to
  print `count 100, min 0, mean 0, p50 0` — a row of zeros that reads exactly
  like a hop measured at sub-nanosecond cost.
- **`backwards`** on the IPC row: 35 of 100 observations had the subscriber's
  `receive` stamp *precede* the publisher's `publish` stamp, so they were
  rejected. Under `Precise` the hop measures tens of microseconds and none are
  rejected; under `Cycle` the readings shrink towards a microsecond, which is
  small enough for the two processes' clock references to disagree about the
  order. Those 35 samples were always being dropped — silently — so the
  surviving `count 65` looked like a complete measurement.

That second row is the case worth internalising: a row can be *wrong* rather
than merely absent, and the only signal is the tally. Three can appear:

| note | meaning | fix |
|---|---|---|
| `same-cycle` | both stamps shared one engine cycle's clock snap | stamp with `Stamping::Precise` |
| `unstamped` | one of the two stages was never stamped | wire the missing stamp |
| `backwards` | the later stamp precedes the earlier one | the two clocks disagree — at this resolution the numbers are not comparable |

## Time source

Stamps always read wall-clock time (never engine time, which is source-driven
in historical mode). Which clock is a `Stamping` value:

- `Stamping::Cycle` reads `Ctx::wall_time()` — a cycle-start snap, one `u64`
  load. Free, but stages that tick in the same engine cycle share the
  timestamp, so the hop between them cannot be measured. Use it for
  cross-process / cross-cycle hops.
- `Stamping::Precise` reads `Ctx::wall_time_precise()` — a fresh TSC read
  (~5–10 ns). Gives intra-cycle resolution so in-process stages get distinct
  timestamps. This example uses it.
- `Stamping::Off` wires no node at all.

`.stamp_as::<X>(mode)` takes that value; `.stamp::<X>()` and
`.stamp_precise::<X>()` are shorthands for the first two. The mode being an
argument is what lets a config flag pick the clock — `Stamping::precise_if(flag)`
— in **one** call. There used to be `.stamp_if(on)` / `.stamp_precise_if(on)`
as well, and they are gone: neither could express "precise or not, decided at
runtime", so that case had to be written as two calls with opposite polarities
(`.stamp_if(!p).stamp_precise_if(p)`), which double-stamps the stage the moment
one `!` is dropped. Turning a stamp off is `Stamping::Off` — or
`Stamping::on_if(flag)` straight from a bool.

On a burst-shaped stream the same method is `.stamp_each_as::<X>(mode)`,
stamping every value in the burst from **one** clock read per stage — a burst
is one instant's worth of values, so a per-value read would invent differences
that do not exist.

Adjacent stages can share a node: `.stamp_all::<(A, B)>(mode)` (and
`.stamp_each_all` on a burst) writes the whole tuple from one op. It is not an
approximation — under `Precise` each stage still takes its own clock read — it
just clones the payload once instead of once per stage, which on a burst is one
`Vec` allocation instead of N.

This pipeline works identically in realtime and historical mode — the same
wiring on a backtest gives you per-stage replay performance, and in production
gives you per-stage latency.

## Toggling

`Stamping::Off` returns the upstream unchanged — no node inserted into the
graph, zero runtime cost. Thread one mode value through your pipeline builder:

```rust
let mode = Stamping::new(cfg.instrument_latency, cfg.precise_stamps);
let pipe = incoming
    .stamp_each_as::<quote_latency::receive>(mode)
    .map(strategy)
    .stamp_each_all::<(quote_latency::strategy, quote_latency::ack)>(mode);
let (sink, latency) = pipe.latency_report_if(mode.is_on(), ReportOutput::Stdout);
```

## Reading the numbers out

`latency_report` hands back a `LatencyHandle`, so the report is not the only
way to see the figures:

```rust
let (_sink, latency) = pipeline.latency_report(ReportOutput::Log);

// After (or during) the run:
for hop in latency.hops() {
    println!("{}: p99 {} ns", hop.label(), hop.p99_ns);
}
println!("end to end p99: {} ns", latency.total().p99_ns);

// Or as a stream, for gauges and alerts — `windows` resets after each read,
// so a p99 tracks the system instead of recording its worst-ever second.
// Because the read is destructive, match the period to whatever consumes it:
// window faster than your scrape and the windows in between are reset before
// anything sees them (see `trading_e2e`'s `LATENCY_WINDOW`).
let per_second = latency.windows(&g, Duration::from_secs(1));
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
[deviation-register](../../../../../docs/planning/deviation-register.md) entry.

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
   report — but the teardown summary fires from graph teardown, which a
   `SIGINT` never reaches, so on that path the report is never printed.
   Passing a duration runs to a clean stop and emits the report; omitting it
   keeps legacy's run-forever behaviour.

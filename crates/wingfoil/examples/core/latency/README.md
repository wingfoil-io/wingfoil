## Per-hop latency in one process

Latency stamps travel with a value, so the same pipeline can explain where it
spent time during a historical replay or a live run. This example keeps the
whole path in one process: it declares three named stages, performs two small
transformations between them, and prints the two hop distributions when the
run ends.

```rust
latency_stages! {
    pub PipelineLatency { received, normalized, decided }
}

let decisions = g
    .ticker(Duration::from_millis(1))
    .count()
    .map(|n: &u64| Traced::<u64, PipelineLatency>::new(*n))
    .stamp_precise::<pipeline_latency::received>()
    .map(|sample: &Traced<u64, PipelineLatency>| {
        Traced::with_latency(sample.payload * 10, sample.latency)
    })
    .stamp_precise::<pipeline_latency::normalized>()
    .map(|sample: &Traced<u64, PipelineLatency>| {
        Traced::with_latency(sample.payload >= 20, sample.latency)
    })
    .stamp_precise::<pipeline_latency::decided>();

let (_sink, latency) = decisions.latency_report(ReportOutput::Stdout);
```

Run it without feature flags or external services:

```sh
cargo run -p wingfoil --example latency
```

The report has two adjacent hops and one end-to-end row:

```text
latency report (delta from previous stage, nanoseconds):
  stage                                 count          min         mean          p50          p99        p99.9          max
  received -> normalized                    5          540         2470          596        10046        10046        10046
  normalized -> decided                     5          536          794          600         1680         1680         1684
  received -> decided (end to end)          5         1076         3265         1192        11648        11648        11730
captured 2 named hops
```

Captured from a debug build on a shared cloud VM, so the nanosecond columns
are a reading of *that* machine — `min` is the closest thing here to the work
itself. What does not vary is the shape: the stage names, their order, the
three rows, and `count 5` on every one of them.

`HistoricalFrom` makes the graph's engine-time schedule reproducible, but
latency measurement intentionally uses wall time: it is measuring how long the
work took, not when the source says the event occurred. That is why the
figures move between runs while the schedule does not.

`Traced::with_latency` is what carries the stamps across a `map`: the payload
changes type — `u64`, then `bool` — while the accumulated latency rides along
untouched.

`stamp_precise` takes a fresh clock reading at each stage. The cheaper `stamp`
uses one wall-clock snapshot per engine cycle, so these three stages, all
reached in one cycle, would be reported as unmeasured rather than as a false
zero-duration hop. Use the precise form for in-process work like this, and the
cycle form for cross-cycle or cross-process boundaries where one read per cycle
is enough — [`showcase/latency`](../../showcase/latency/) is that case, over a
real transport, and its report shows exactly what `stamp` elides.

The returned `LatencyHandle` is not print-only. Here it supplies the final hop
count; applications can also read snapshots or expose rolling windows to
metrics and alerting code.

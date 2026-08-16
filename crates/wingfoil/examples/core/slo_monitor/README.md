## SLO burn-rate monitoring

A service-reliability graph: request events arrive from a producer thread, and
the graph turns them into an error-budget burn rate, a latency profile and a
throughput gauge — then pages when the burn rate says the budget will not last,
and clears when it recovers.

It is the monitoring shape rather than the trading one, but the structure is
the same as [`ema_crossover`](../ema_crossover/): a live-shaped feed in, rolling
statistics over it, and a state machine emitting events at the tail. What it
adds is the two constructs that a straight `map`/`fold` chain cannot express —
a **feedback edge** for hysteresis, and a **clock merged into a time window** so
a rate decays when traffic stops.

```rust
// One SLO covers one endpoint.
let (incoming, tx) = g.channel::<Request>();
let requests = incoming.collapse::<Request>().filter_value(|r| r.endpoint == ENDPOINT);

// Burn rate: how many times faster than budget the errors are arriving.
let violation = requests.map(|r| if r.status >= 500 || r.latency_ms > SLO_LATENCY_MS { 1.0 } else { 0.0 });
let burn = violation.rolling_mean(WINDOW).map(|r| r / ERROR_BUDGET);

// Hysteresis: page above PAGE, clear below CLEAR, hold in between.
let (was_alerting, alert_sink) = g.feedback::<bool>();
let alerting = burn
    .join_passive(&was_alerting, |b, prev| if *prev { *b > CLEAR } else { *b > PAGE })
    .feedback(&alert_sink);
let transitions = alerting.distinct().skip(1);
```

Traffic runs at 500 rps and degrades between t=2s and t=3s. The page fires as
the bad window fills, and clears once the good requests have flushed it:

```text
replaying 3000 requests to /checkout (plus /health noise) ...
[  0.0s]        rps=   1  p50=  40.0ms  max=  40.0ms
[  1.0s]        rps= 501  p50=  67.5ms  max=  99.0ms
[  2.0s]        rps= 501  p50=  71.5ms  max= 380.0ms
[  2.0s] PAGE  /checkout  burn=15.6x budget
[  3.0s]        rps= 501  p50= 447.5ms  max= 479.0ms
[  3.1s] CLEAR /checkout  burn=4.7x budget
[  4.0s]        rps= 501  p50=  67.5ms  max=  99.0ms
[  5.0s]        rps= 501  p50=  71.5ms  max=  99.0ms
[  6.0s]        rps= 500  p50=  71.5ms  max=  99.0ms
[  7.0s]        rps=   0  p50=  71.5ms  max=  99.0ms
[  8.0s]        rps=   0  p50=  71.5ms  max=  99.0ms
```

The run is bounded by `RunFor::Duration(7s)` but a line lands at 8.0s. That is
deliberate: in `Kernel::begin_cycle` a reached time bound sets `is_last_cycle`
rather than ending the run there, so the next scheduled cycle still runs and the
run stops on the one after. A `RunFor::Cycles` bound terminates immediately
instead — only the time bound grants that final cycle.

### Idiom notes

- **A `time_windowed_*` op evicts only when its input ticks.** Its activation is
  `NONE`, and the eviction pass runs inside the same update that pushes a new
  sample — so once traffic stops the op holds its last value indefinitely rather
  than decaying to zero. Merging the status clock in as a `0.0` sample fixes it
  for a *sum*: the zero does not change the total but does force the eviction.
  A mean or median cannot be corrected this way, because the injected sample
  would enter the statistic.
- **`distinct()` emits its first value.** As an edge detector that means one
  spurious transition at the start, which `skip(1)` drops.
- **Close the sender.** A historical run block-collects its input at `start`, so
  a channel whose sender is still alive and silent blocks there forever —
  `RunFor::Cycles`/`Duration` cannot bound a wait that happens before the first
  cycle. `tx.close()`, or dropping the sender, is what ends the replay.
- `filter_value` takes a predicate on the value; `filter` takes a separate
  `Stream<bool>` to gate on. Both are useful — this graph uses the first for the
  endpoint filter, and the burn-rate state machine is a `join_passive` rather
  than either.

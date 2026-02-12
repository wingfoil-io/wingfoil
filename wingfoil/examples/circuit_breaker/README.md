# Circuit Breaker Example

We mock up price data, by constructing a source ticks every 100us 
producing the sequence (1, 2, 3, ...).  We then compare the current 
value against a delayed version of itself to detect change. 
When the difference exceeds a threshold, the breaker fires, 
resetting the lookback window. This prevents excessive
re-triggering while still enforcing the control.

## Feedback

Feedback allows a downstream result to influence an upstream node on the
next engine cycle, without creating a cycle in the DAG. The graph engine
sees the feedback source as having no upstreams, so topological ordering
is preserved.

There are two flavours:

- `feedback_node()` creates a channel carrying `()` — a pure tick signal
  with no value. Useful when you only need to signal that something
  happened (e.g. a reset trigger), as in this example.
- `feedback::<T>()` creates a channel carrying values of type `T`.
  The returned `FeedbackSink<T>` can be moved into closures and the
  paired `Rc<dyn Stream<T>>` can be used as a passive or active upstream.
  See the `feedback_passive_works` and `feedback_active_works` tests in
  `src/nodes/feedback.rs` for value-carrying feedback examples.


```
ticker -> count -> bimap(diff) -> filter(|diff| > level) -> feedback_node(tx)
                     ^                                            |
                     |                                            v
                  delay_with_reset  <-----------------------------+
                     (rx)
```

## `delay_with_reset`

`delay_with_reset(duration, trigger)` works like `delay` — it emits the
source value after `duration` has elapsed. The difference is the trigger
input: when the trigger fires, the output snaps to the current source
value and the pending queue is cleared. This gives us the reset
mechanism the circuit breaker needs.

## Active vs Passive Dependencies

In the `bimap` call, the source is an **Active** dependency and the
delayed stream is **Passive**:

```rust
let diff = bimap(Dep::Active(source), Dep::Passive(delayed), |a, b| {
    a as i64 - b as i64
});
```

- **Active** — when this upstream ticks, it triggers the downstream node
  to cycle. The source counter drives the diff calculation.
- **Passive** — the value is read when the downstream cycles, but ticking
  alone does not trigger it. The delayed stream updates in the background
  (and snaps on reset) but we only evaluate the diff when the source ticks.

This distinction matters here because after a reset, `delay_with_reset`
ticks with the snapped value. If delayed were Active it would cause an
extra diff evaluation at that moment, which is not what we want — we
only want to compute the diff in lockstep with the source.

## Running

```bash
RUST_LOG=info cargo run --example circuit_breaker
```

## Code

```rust
use log::Level::Info;
use wingfoil::*;
use std::time::Duration;

env_logger::init();
let period = Duration::from_micros(100);
let lookback = 5;
let level: i64 = 3;

// Source: a counter ticking every 100us
let source = ticker(period).count();

// Create a feedback channel carrying () — just a tick signal.
let (tx, rx) = feedback_node();

// delay_with_reset emits the source value delayed by `lookback` periods.
// When the trigger (rx) fires, the delay resets: the output snaps to the
// current source value and the pending queue is cleared.
let delayed = source.delay_with_reset(period * lookback, rx);

// The delayed stream is a Passive dependency — it is read when diff ticks
// but does not itself trigger diff. Only the source (Active) triggers it.
// This matters because after a reset, delayed ticks with the snapped value;
// if it were Active it would cause an extra diff evaluation at that moment.
let diff = bimap(Dep::Active(source), Dep::Passive(delayed), |a, b| {
    a as i64 - b as i64
})
.logged("diff", Info);

// When |diff| exceeds the level, fire the feedback to reset the delay
let trigger = diff
    .filter_value(move |p| p.abs() > level)
    .logged("trigger", Info)
    .feedback_node(tx);

Graph::new(
    vec![trigger],
    RunMode::HistoricalFrom(NanoTime::ZERO),
    RunFor::Duration(period * 14),
)
.run()
.unwrap();
```

## Output

```
[INFO  wingfoil] 0.000_000 diff 0
[INFO  wingfoil] 0.000_100 diff 1
[INFO  wingfoil] 0.000_200 diff 2
[INFO  wingfoil] 0.000_300 diff 3
[INFO  wingfoil] 0.000_400 diff 4
[INFO  wingfoil] 0.000_400 trigger 4      <- breaker fires, delay resets
[INFO  wingfoil] 0.000_500 diff 1
[INFO  wingfoil] 0.000_600 diff 2
[INFO  wingfoil] 0.000_700 diff 3
[INFO  wingfoil] 0.000_800 diff 4
[INFO  wingfoil] 0.000_800 trigger 4      <- breaker fires again
[INFO  wingfoil] 0.000_900 diff 1
[INFO  wingfoil] 0.001_000 diff 2
[INFO  wingfoil] 0.001_100 diff 3
[INFO  wingfoil] 0.001_200 diff 4
[INFO  wingfoil] 0.001_200 trigger 4      <- and again
[INFO  wingfoil] 0.001_300 diff 1
[INFO  wingfoil] 0.001_400 diff 2
[INFO  wingfoil] 0.001_500 diff 3
```

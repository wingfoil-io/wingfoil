## Feedback Example

A wingfoil graph is a *directed acyclic* graph: you build it in dependency order
and the scheduler walks it one way, from sources to sinks. A `feedback` channel
lets you close a loop between two nodes — where A's input depends on B and B's
input depends on A — which a DAG can't otherwise express.

`feedback::<T>()` returns a pair:

- `tx` — a `FeedbackSink<T>`, the **write** end. Cloneable, movable into closures.
- `rx` — a `Stream<T>` (or `Node`, via `feedback_node()`). It has no upstreams,
  so the graph stays acyclic. A value written via `tx` is emitted on the next
  engine tick — a one-step delay, exactly the sample delay a discrete control
  loop expects.

Because `rx` gives you the read end of a value *before* that value is produced,
you can reference it upstream of the node that computes it.

### A proportional control loop

This example regulates a room's temperature to a setpoint with a proportional
controller. The loop runs through three nodes:

```text
                 ┌──────────── feedback (temperature) ───────────┐
                 │                                                │
                 ▼                                                │
   setpoint ─▶ ┌───────┐   error   ┌────────┐  heater  ┌───────┐  │
               │ error │ ────────▶ │ heater │ ───────▶ │ plant │ ─┘
   clock ────▶ └───────┘           └────────┘          └───────┘
              (controller)      (control signal)        (room)
```

`error` needs the temperature, but the temperature is produced by `plant`,
downstream of `error`. The feedback channel carries it back around the loop.

```rust,ignore
use std::time::Duration;
use wingfoil::*;

let period = Duration::from_secs(1);
let setpoint = 20.0_f64;
let gain = 0.5_f64;

// A clock to advance the loop one step per period.
let clock = ticker(period).count();

// temp_rx lets us reference the temperature before the plant produces it.
let (temp_tx, temp_rx) = feedback::<f64>();

// Controller: distance from setpoint, then how hard to drive the heater.
let error = bimap(
    Dep::Active(clock),
    Dep::Passive(temp_rx.clone()),
    move |_, temp| setpoint - temp,
);
let heater = error.map(move |e| gain * e);

// Plant (the room): new temperature = old temperature + heater output.
let plant = bimap(Dep::Active(heater), Dep::Passive(temp_rx), |power, temp| temp + power);

// Close the loop by feeding each new temperature back.
plant
    .feedback(temp_tx)
    .for_each(|t, _| println!("temperature: {t:.3}"))
    .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Duration(period * 7))?;
```

Run it:

```bash
cargo run --example feedback
```

The temperature converges on the setpoint, each step closing half the remaining
gap:

```text
temperature: 10.000
temperature: 15.000
temperature: 17.500
temperature: 18.750
temperature: 19.375
temperature: 19.688
temperature: 19.844
temperature: 19.922
```

See the [`feedback` API docs](https://docs.rs/wingfoil/latest/wingfoil/fn.feedback.html)
for the full surface, including `feedback_node()` for tick-only reset signals
(often paired with
[`delay_with_reset`](https://docs.rs/wingfoil/latest/wingfoil/trait.StreamOperators.html)).

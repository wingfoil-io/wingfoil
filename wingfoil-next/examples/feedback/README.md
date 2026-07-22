## Feedback loops

A directed acyclic graph cannot express a cycle directly — but real systems
have them. A feedback edge breaks the cycle in time: values sent to the sink
arrive on the paired source *one cycle later*, so the graph stays acyclic while
the loop still closes.

This is the fluent-API port of the classic `feedback` example: a proportional
temperature controller. The room (the "plant") is heated toward a setpoint; the
controller reads the current temperature, computes the error, and drives the
heater proportionally. The new temperature feeds back for the next step.

```rust
let (temp_rx, temp_tx) = g.feedback::<f64>();
let clock = g.ticker(period).count();

// error and plant read the fed-back temperature *passively* (join_passive):
// the clock / heater drive the tick, the temperature is only read.
let error = clock.join_passive(&temp_rx, move |_, temp| setpoint - temp);
let heater = error.map(move |e| gain * e);
let plant = heater.join_passive(&temp_rx, |power, temp| temp + power);

// Close the loop.
let temperature = plant.feedback(&temp_tx);
```

Starting from 0, the temperature converges geometrically toward the setpoint of
20:

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

### Idiom notes

- `g.feedback::<T>()` returns `(source, sink)` — the reverse of the classic
  engine's `(tx, rx)` ordering.
- The classic engine used `bimap(Dep::Active, Dep::Passive, …)`; the next engine
  spells the same active/passive combine as `join_passive`.
- `stream.feedback(&sink)` is a pass-through that also forwards each value to the
  sink for delivery next cycle.

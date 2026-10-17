## Feedback loops

A directed acyclic graph cannot express a cycle directly — but real systems
have them. A feedback edge breaks the cycle in time: values sent to the sink
arrive on the paired source *one cycle later*, so the graph stays acyclic while
the loop still closes.

This is the fluent-API port of the legacy `feedback` example: a proportional
temperature controller. The room (the "plant") is heated toward a setpoint; the
controller reads the current temperature, computes the error, and drives the
heater proportionally. The new temperature feeds back for the next step.

```rust
let clock = g.ticker(period).count();

// `temp_was` is the temperature from the step before; the body names the new
// temperature as `feedback`, which is sent round to arrive one cycle later.
let (temperature, heater) = g.feedback_loop(|temp_was| {
    // error and plant read the fed-back temperature *passively*
    // (join_passive): the clock / heater drive the tick, the temperature is
    // only read.
    let error = clock.join_passive(temp_was, move |_, temp: &f64| setpoint - temp);
    let heater = error.map(move |e| gain * e);
    let temperature = heater.join_passive(temp_was, |power, temp: &f64| temp + power);
    // `feedback` goes round the loop; `out` (the heater) is handed back
    // beside it, so it can be printed outside the loop too.
    FeedbackLoop { feedback: temperature, out: heater }
});
```

Starting from 0, the temperature converges geometrically toward the setpoint of
20 as the heater backs off:

```text
heater: 10.000  temperature: 10.000
heater: 5.000  temperature: 15.000
heater: 2.500  temperature: 17.500
heater: 1.250  temperature: 18.750
heater: 0.625  temperature: 19.375
heater: 0.312  temperature: 19.688
heater: 0.156  temperature: 19.844
heater: 0.078  temperature: 19.922
```

### Idiom notes

- `g.feedback_loop(|was| ..)` opens the edge, hands the body what was sent last
  cycle, and closes the edge on the stream the body returns, so the loop cannot
  be left open and its type is inferred. The body returns either that stream
  alone — when it is all that is wanted outside the loop — or, as here,
  `FeedbackLoop { feedback: stream, out: other }` when it builds something
  else wanted outside too: `feedback` goes round, `out` does not, and both
  come back as a `(stream, other)` tuple.
- The lower-level pair is still there for a loop one closure cannot hold:
  `g.feedback::<T>()` returns `(source, sink)` — the reverse of the legacy
  engine's `(tx, rx)` ordering — and `stream.feedback(&sink)` is a pass-through
  that also forwards each value to the sink for delivery next cycle.
- The legacy engine used `bimap(Dep::Active, Dep::Passive, …)`; the wingfoil engine
  spells the same active/passive combine as `join_passive`.

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

// `temp_was` is the temperature from the step before; the body returns the
// new temperature, which is sent round to arrive one cycle later.
let temperature = g.feedback_loop(|temp_was| {
    // error and plant read the fed-back temperature *passively*
    // (join_passive): the clock / heater drive the tick, the temperature is
    // only read.
    let error = clock.join_passive(temp_was, move |_, temp: &f64| setpoint - temp);
    let heater = error.map(move |e| gain * e);
    heater.join_passive(temp_was, |power, temp: &f64| temp + power)
});
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

- `g.feedback_loop(|was| ..)` opens the edge, hands the body what was sent last
  cycle, and closes the edge on the stream the body returns, so the loop cannot
  be left open and its type is inferred. A body that builds something else
  wanted outside the loop returns `FeedbackLoop { feedback: stream, out: other }` —
  `feedback` goes round, `out` does not — and gets both back as a
  `(stream, other)` tuple.
- The lower-level pair is still there for a loop one closure cannot hold:
  `g.feedback::<T>()` returns `(source, sink)` — the reverse of the legacy
  engine's `(tx, rx)` ordering — and `stream.feedback(&sink)` is a pass-through
  that also forwards each value to the sink for delivery next cycle.
- The legacy engine used `bimap(Dep::Active, Dep::Passive, …)`; the wingfoil engine
  spells the same active/passive combine as `join_passive`.

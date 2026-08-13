## EMA crossover — a backtest-shaped graph

A realistic-shaped backtest built from four primitives: `fold` (stateful),
`join` (two-input combine), `map`, and `filter`. A deterministic pseudo-random
price walk feeds a fast and a slow EMA; when the fast crosses the slow, a
golden/death-cross event is emitted — but **only on the change**, not on every
tick where the condition happens to hold.

The price walk is an LCG living in `fold` state, so the whole run is
reproducible: same numbers every time, which is what makes a backtest a test.

```rust
let tick  = g.ticker(Duration::from_millis(1));
let price = tick.fold((seed, 100.0_f64), lcg_walk).map(|st| st.1);

// Fast and slow EMAs over the same price stream, recombined into a signal.
let fast   = price.fold((0.0, false), ema(0.30)).map(|s| s.0);
let slow   = price.fold((0.0, false), ema(0.05)).map(|s| s.0);
let signal = fast.join(&slow, |f, s| f > s);

// Edge detector: keep the previous value beside the current one, and emit
// only when they differ.
let changed = signal
    .fold((false, false), |st, s| { st.0 = st.1; st.1 = *s; })
    .map(|st| st.0 != st.1);

let events = count.join(&signal, format_event).filter(&changed);

// The outbound edge: print each crossover as it happens, and let the graph
// carry the running total.
let printed  = events.for_each(|e: &String| { println!("  {e}"); Ok(()) });
let n_events = printed.count();
```

### The report is a stream, not a `Vec`

Events are printed from a `for_each` sink as they are produced, not collected
with `accumulate()` and dumped after the run. That is what makes the same wiring
point at a live feed unchanged: an accumulator grows one entry per event for the
whole run, which a backtest survives and a deployed graph does not. `count()` on
the sink's tick stream gives the total without keeping the events around.

`price` is read by both EMAs — a **shared node**. The topologically sorted
scheduler runs it once per cycle and fans the tick out to both readers, rather
than once per downstream path; see
[`topological_sort`](../topological_sort/) for why that matters as graphs get
wider.

### The edge-detector idiom

`filter(&changed)` takes a *stream* of booleans, not a closure — the gate is
itself a node in the graph. This is the standard way to express "emit on state
change" without any node needing to remember whether it has already fired:
`fold` carries `(previous, current)` and `map` compares them.

### Output

```text
backtest: 2000 ticks — crossover events:
  t=   4ms  golden cross -> LONG
  t=  10ms  death cross  -> FLAT
  t=  20ms  golden cross -> LONG
  t=  61ms  death cross  -> FLAT
  t= 110ms  golden cross -> LONG
  ...
  t=1941ms  golden cross -> LONG
  t=1947ms  death cross  -> FLAT
fast EMA 98.15 vs slow EMA 98.69 at close — 88 crossover events
```

### Run

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --example ema_crossover
```

### Where to go next

- [`statistics`](../statistics/) — the same rolling maths from the
  `StatisticsOps` trait instead of hand-rolled `fold`s.
- [`order_book`](../order_book/) — heavier state in `fold`.
- [`run_mode`](../run_mode/) — point this wiring at a live feed instead.

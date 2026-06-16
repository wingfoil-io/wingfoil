# Wingfoil Live Demo — Recording Script

A ~3–5 minute, live-coded YouTube demo. You build a moving-average **crossover
trading signal** from scratch using wingfoil's fluent API, run it in real time,
then flip a single line to backtest it. The finale reveals the same ideas
running on a full day of real NASDAQ order data.

**Audience:** Rust developers (new to wingfoil) with a trading flavour.
**Promise to the viewer:** "By the end you'll have written a streaming trading
signal in ~20 lines of Rust, and run it as both a live feed and a backtest."

---

## Before you hit record

```bash
# Warm the build cache so there's no compile wait on camera:
cargo build --example crossover
cargo build --release --example order_book --features csv

# Have two files open in the editor:
#   wingfoil/examples/crossover/main.rs   <- you type into this
#   wingfoil/examples/order_book/main.rs  <- the finale reveal
```

- Terminal font large (18pt+), window ~100 cols wide.
- Start `crossover/main.rs` **empty** (the finished version lives in git — copy
  it somewhere safe, then clear the file to type it live).
- Keep `cargo run --example crossover` in your shell history (up-arrow to re-run).

---

## The arc (4 stages, each one compiles and runs)

### Stage 1 — "Hello, stream" (~45s)

> *Talking point:* "Wingfoil builds a graph of streaming computations. The
> simplest source is a `ticker` — a clock. Let's count its ticks."

```rust
use std::time::Duration;
use wingfoil::*;

fn main() -> anyhow::Result<()> {
    ticker(Duration::from_millis(20))
        .count()
        .for_each(|n, time| println!("{}   tick {n}", time.pretty()))
        .run(RunMode::RealTime, RunFor::Cycles(20))?;
    Ok(())
}
```

```bash
cargo run --example crossover
```

> *Land it:* "Notice the timestamps advance in real time — this is running on
> the wall clock. Hold that thought."

### Stage 2 — A synthetic price (~45s)

> *Talking point:* "`map` transforms each value. Let's turn the tick count into
> a price that drifts up and oscillates — a stand-in for a live market feed."

```rust
    let prices = ticker(Duration::from_millis(20))
        .count()
        .map(|n| 100.0 + 15.0 * ((n as f64) / 25.0).sin() + (n as f64) * 0.02);

    prices
        .for_each(|p, time| println!("{}   price {p:7.2}", time.pretty()))
        .run(RunMode::RealTime, RunFor::Cycles(150))?;
```

```bash
cargo run --example crossover
```

> *Land it:* "There's our price stream, ticking 50 times a second."

### Stage 3 — Two moving averages + the crossover (~90s — the payoff)

> *Talking point:* "Now the real thing. `fold` carries state across ticks, so I
> can compute an exponential moving average — `new = old + alpha * (price -
> old)`. A fast EMA reacts quickly; a slow one lags. When fast crosses above
> slow, we're LONG; when it crosses back, FLAT. The classic golden/death cross."

```rust
    let ema = |alpha: f64| {
        move |ema: &mut f64, price: f64| {
            *ema = if *ema == 0.0 { price } else { *ema + alpha * (price - *ema) };
        }
    };
    let fast = prices.fold(ema(0.30));
    let slow = prices.fold(ema(0.05));

    let report = trimap(
        Dep::Active(prices.clone()),
        Dep::Passive(fast.clone()),
        Dep::Passive(slow.clone()),
        |price, fast, slow| {
            let signal = if fast > slow { "^ LONG" } else { "v FLAT" };
            format!("price {price:7.2}   fast {fast:7.2}   slow {slow:7.2}   {signal}")
        },
    );

    report
        .for_each(|line, time| println!("{}   {line}", time.pretty()))
        .run(RunMode::RealTime, RunFor::Cycles(150))?;
```

```bash
cargo run --example crossover
```

> *Land it:* "Watch the fast EMA pull away from the slow one — and the moment
> they cross, the signal flips. That's a trading strategy, live, in ~20 lines."
>
> *Concept callouts (pick one, keep it short):*
> - **`fold` = stateful operator.** "The EMA's running value lives between ticks."
> - **Active vs passive.** "`prices` is the *active* trigger; the two EMAs are
>   *passive* — read, but they don't re-fire the node. Wingfoil still runs them
>   in the right order. That's the DAG engine doing the scheduling for you."

### Stage 4 — Same graph, instant backtest (~30s)

> *Talking point:* "Here's the part traders care about. I change **one line** —
> the run mode — and the exact same graph becomes a backtest. No wall-clock
> wait; it runs as fast as the CPU allows."

```rust
    // was: RunMode::RealTime
    report
        .for_each(|line, time| println!("{}   {line}", time.pretty()))
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(150))?;
```

```bash
cargo run --example crossover
```

> *Land it:* "Same wiring, same output — but instant. Real time for production,
> historical for backtesting and tests. Time is a first-class citizen here."

---

## Finale — real market data (~45s)

Switch to `wingfoil/examples/order_book/main.rs`.

> *Talking point:* "Synthetic prices are fine for a demo. But wingfoil ships
> with I/O adapters, so the same model plugs into real data. This example loads
> **91,998 real NASDAQ AAPL order messages** from a CSV, maintains a live order
> book, derives trades and two-way prices, and writes them back out — and the
> inputs and outputs all tick at different frequencies."

```bash
cargo run --release --example order_book --features csv
```

> *Land it (show the printed graph + the output files):*
> - "A whole trading day processed in well under a second."
> - "Out come **15,041 price updates** and **4,170 fills** — `prices.csv` and
>   `fills.csv`."
> - "Notice wingfoil printed the graph it built and ran. Same fluent API you
>   just saw — `map`, `filter`, `split`, `distinct` — wired to a real adapter
>   with one line: `csv_read(...)` / `.csv_write(...)`."

---

## Close (~20s)

> "So: streams in, a DAG of transforms, real-time or historical with one line,
> and adapters for real data. That's wingfoil. It's on crates.io, there are
> Python and TypeScript clients, and a pile more examples in the repo. Links
> below — thanks for watching."

Links to drop in the description:
- crates.io: https://crates.io/crates/wingfoil
- docs: https://docs.rs/wingfoil
- repo + examples: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples
- discord: https://discord.gg/rfGqf3Ff

---

## Timing budget

| Segment | Target |
|---|---|
| Stage 1 — hello stream | 0:45 |
| Stage 2 — synthetic price | 0:45 |
| Stage 3 — crossover (payoff) | 1:30 |
| Stage 4 — backtest flip | 0:30 |
| Finale — real AAPL data | 0:45 |
| Close | 0:20 |
| **Total** | **~4:35** |

## Tips

- If live typing is risky, pre-type each stage and reveal with editor snippets;
  the `cargo run` is what sells it, not the keystrokes.
- The finished `crossover` example (all stages combined) is committed at
  `wingfoil/examples/crossover/` — rehearse against it and copy from it.
- `for_each` streams output live; `print()` buffers until the end. Use
  `for_each` on camera so the terminal scrolls as it runs.
- All numbers above are real, measured on this repo (2026-06-16).

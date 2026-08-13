//! A realistic-shaped backtest in the fluent style: a deterministic
//! pseudo-random price walk, fast/slow EMAs, and golden/death-cross signals
//! emitted only when the crossover state *changes* — exercising `fold`
//! (stateful), `join` (two-input combine), `map`, and `filter`.
//!
//! ```sh
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --example ema_crossover
//! ```

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// EMA update, seeded by the first observation instead of decaying from 0.
fn ema(alpha: f64) -> impl Fn(&mut (f64, bool), &f64) {
    move |state, price| {
        if state.1 {
            state.0 += alpha * (price - state.0);
        } else {
            *state = (*price, true);
        }
    }
}

fn main() {
    let g = GraphBuilder::new();
    let tick = g.ticker(Duration::from_millis(1));
    let count = tick.count();

    // Deterministic price walk: an LCG in fold state drives ±0.5 steps
    // around 100.0.
    let price = tick
        .fold((0x2545_F491_4F6C_DD1D_u64, 100.0_f64), |st, _| {
            st.0 =
                st.0.wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
            let unit = ((st.0 >> 33) as f64 / (1u64 << 30) as f64) - 1.0; // [-1, 1)
            st.1 += unit * 0.5;
        })
        .map(|st| st.1);

    // Fast and slow EMAs over the same price stream.
    let fast = price.fold((0.0, false), ema(0.30)).map(|s| s.0);
    let slow = price.fold((0.0, false), ema(0.05)).map(|s| s.0);

    // Crossover signal, and an edge detector so we only emit *changes*.
    let signal = fast.join(&slow, |f, s| f > s);
    let changed = signal
        .fold((false, false), |st, s| {
            st.0 = st.1;
            st.1 = *s;
        })
        .map(|st| st.0 != st.1);

    // Human-readable event, gated on the edge.
    let events = count
        .join(&signal, |i, long| {
            if *long {
                format!("t={i:>4}ms  golden cross -> LONG")
            } else {
                format!("t={i:>4}ms  death cross  -> FLAT")
            }
        })
        .filter(&changed);

    // The outbound edge: print each crossover as it happens. A backtest report
    // is a *stream* of events, so it belongs in a `for_each` sink — the same
    // wiring then works unchanged against a live feed, where accumulating
    // every event into a `Vec` would grow without bound.
    let printed = events.for_each(|e: &String| {
        println!("  {e}");
        Ok(())
    });
    // `count` on the sink's tick stream: the running event total, carried by
    // the graph rather than by a `Vec` length read afterwards.
    let n_events = printed.count();

    let mut runner = g.build();
    println!("backtest: 2000 ticks — crossover events:");
    runner
        .run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(2_000),
        )
        .unwrap();

    println!(
        "fast EMA {:.2} vs slow EMA {:.2} at close — {} crossover events",
        runner.value(&fast),
        runner.value(&slow),
        runner.value(&n_events),
    );
}

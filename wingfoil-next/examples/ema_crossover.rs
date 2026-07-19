//! A realistic-shaped backtest on the prototype: a deterministic
//! pseudo-random price walk, fast/slow EMAs, and golden/death-cross signals
//! emitted only when the crossover state *changes* — exercising `fold`
//! (stateful), `join` (two-input combine), `map`, and `filter`.
//!
//! ```sh
//! cargo run -p wingfoil-next --example ema_crossover
//! ```

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::interp::Builder;

/// EMA update, seeded by the first observation instead of decaying from 0.
fn ema(alpha: f64) -> impl FnMut(&mut (f64, bool), &f64) {
    move |state, price| {
        if state.1 {
            state.0 += alpha * (price - state.0);
        } else {
            *state = (*price, true);
        }
    }
}

fn main() {
    let mut g = Builder::new();
    let tick = g.ticker(Duration::from_millis(1));

    // Tick index, for labelling signals.
    let count = g.fold(tick, 0u64, |acc, _: &()| *acc += 1);

    // Deterministic price walk: an LCG in fold state drives ±0.5 steps
    // around 100.0.
    let walk = g.fold(
        tick,
        (0x2545_F491_4F6C_DD1D_u64, 100.0_f64),
        |st, _: &()| {
            st.0 =
                st.0.wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
            let unit = ((st.0 >> 33) as f64 / (1u64 << 30) as f64) - 1.0; // [-1, 1)
            st.1 += unit * 0.5;
        },
    );
    let price = g.map(walk, |st: &(u64, f64)| st.1);

    // Fast and slow EMAs over the same price stream.
    let fast_state = g.fold(price, (0.0, false), ema(0.30));
    let slow_state = g.fold(price, (0.0, false), ema(0.05));
    let fast = g.map(fast_state, |s: &(f64, bool)| s.0);
    let slow = g.map(slow_state, |s: &(f64, bool)| s.0);

    // Crossover signal, and an edge detector so we only emit *changes*.
    let signal = g.join(fast, slow, |f: &f64, s: &f64| f > s);
    let edge = g.fold(signal, (false, false), |st, s: &bool| {
        st.0 = st.1;
        st.1 = *s;
    });
    let changed = g.map(edge, |st: &(bool, bool)| st.0 != st.1);

    // Human-readable event, gated on the edge.
    let described = g.join(count, signal, |i: &u64, long: &bool| {
        if *long {
            format!("t={i:>4}ms  golden cross -> LONG")
        } else {
            format!("t={i:>4}ms  death cross  -> FLAT")
        }
    });
    let events = g.filter(described, changed);
    let log = g.fold(events, Vec::new(), |acc: &mut Vec<String>, v: &String| {
        acc.push(v.clone())
    });

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Cycles(2_000),
    );

    let events = runner.value(log);
    println!(
        "backtest: 2000 ticks, fast EMA {:.2} vs slow EMA {:.2} at close — {} crossover events:",
        runner.value(fast),
        runner.value(slow),
        events.len()
    );
    for e in &events {
        println!("  {e}");
    }
}

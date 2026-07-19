//! The dual-mode thesis in one file: the same graph — summing even tick
//! counts — executed by the interpreted engine and by a compiled runner
//! (hand-expanded, exactly what a `graph!` macro would emit), both calling
//! the *identical* `Op::cycle` functions. Prints the results (which must
//! agree) and a rough timing comparison.
//!
//! ```sh
//! cargo run -p wingfoil-next --release --example dual_mode
//! ```

use std::time::{Duration, Instant};

use wingfoil::codegen::Kernel;
use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::fluent::GraphBuilder;
use wingfoil_next::op::{Ctx, Op, Tick};
use wingfoil_next::ops::{Filter, Fold, Map, Ticker};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const CYCLES: u32 = 200_000;
const PERIOD: Duration = Duration::from_micros(1);

/// Graph: ticker → count → is_even → filter evens → running sum.
fn interpreted() -> u64 {
    let g = GraphBuilder::new();
    let count = g.ticker(PERIOD).count();
    let is_even = count.map(|i| i.is_multiple_of(2));
    let sum = count.filter(&is_even).fold(0u64, |acc, v| *acc += v);
    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(CYCLES));
    runner.value(&sum)
}

/// The same graph as a compiled runner: state in locals, tick propagation as
/// bools, every op call monomorphized — including the closures, which exist
/// in exactly one place per graph once a macro emits both modes.
fn compiled() -> u64 {
    let mut tick_cfg = NanoTime::from(PERIOD);
    let mut tick_state: Option<NanoTime> = None;
    let mut count_f = |acc: &mut u64, _: &()| *acc += 1;
    let mut count_acc = 0u64;
    let mut even_f = |i: &u64| i.is_multiple_of(2);
    let mut sum_f = |acc: &mut u64, v: &u64| *acc += v;
    let mut sum_acc = 0u64;

    let mut v_count = 0u64;
    let mut v_is_even = false;
    let mut v_evens = 0u64;

    let mut k = Kernel::new(HISTORICAL, RunFor::Cycles(CYCLES));
    {
        let mut ctx = Ctx::new(&mut k, 0);
        Ticker::start(&mut tick_cfg, &mut tick_state, &mut ctx);
    }
    let mut dirty = [false; 5];
    while k.begin_cycle(&mut dirty) {
        let t_tick = dirty[0] && {
            let mut ctx = Ctx::new(&mut k, 0);
            matches!(
                Ticker::cycle(&mut tick_cfg, &mut tick_state, (), &mut ctx),
                Tick::Value(())
            )
        };
        let t_count = t_tick && {
            let mut ctx = Ctx::new(&mut k, 1);
            match <Fold<(), u64, _>>::cycle(&mut count_f, &mut count_acc, (&(),), &mut ctx) {
                Tick::Value(v) => {
                    v_count = v;
                    true
                }
                Tick::Quiet => false,
            }
        };
        let t_is_even = t_count && {
            let mut ctx = Ctx::new(&mut k, 2);
            match <Map<u64, bool, _>>::cycle(&mut even_f, &mut (), (&v_count,), &mut ctx) {
                Tick::Value(v) => {
                    v_is_even = v;
                    true
                }
                Tick::Quiet => false,
            }
        };
        let t_evens = (t_count || t_is_even) && {
            let mut ctx = Ctx::new(&mut k, 3);
            match <Filter<u64>>::cycle(&mut (), &mut (), (&v_count, &v_is_even), &mut ctx) {
                Tick::Value(v) => {
                    v_evens = v;
                    true
                }
                Tick::Quiet => false,
            }
        };
        if t_evens {
            let mut ctx = Ctx::new(&mut k, 4);
            let _ = <Fold<u64, u64, _>>::cycle(&mut sum_f, &mut sum_acc, (&v_evens,), &mut ctx);
        }
        k.end_cycle(&mut dirty);
    }
    sum_acc
}

fn main() {
    let t = Instant::now();
    let a = interpreted();
    let interp_time = t.elapsed();

    let t = Instant::now();
    let b = compiled();
    let compiled_time = t.elapsed();

    assert_eq!(a, b, "engines must agree");
    println!("sum of even counts over {CYCLES} cycles: {a} (both engines agree)");
    println!(
        "interpreted: {interp_time:?}  ({:.1} ns/cycle)",
        interp_time.as_nanos() as f64 / CYCLES as f64
    );
    println!(
        "compiled:    {compiled_time:?}  ({:.1} ns/cycle)",
        compiled_time.as_nanos() as f64 / CYCLES as f64
    );
    println!(
        "speedup:     {:.1}x  (run with --release for representative numbers)",
        interp_time.as_secs_f64() / compiled_time.as_secs_f64()
    );
}

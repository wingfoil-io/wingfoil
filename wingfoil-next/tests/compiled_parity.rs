//! The load-bearing test of the prototype: the odds/evens graph executed by
//! the interpreted engine and by a hand-expanded compiled runner — both
//! calling the *same* `Op::cycle` functions — must agree exactly.
//!
//! `compiled_odds_evens` is written the way a `graph!` proc macro would
//! expand it: cfg/state/values as locals, tick propagation as `bool`s, every
//! op call monomorphized (closures included — they are `Cfg` type
//! parameters, so LLVM sees straight through them). Note what is *absent*
//! compared to the main crate's generated runners: no re-implemented node
//! semantics, no downcasts, no `RefCell`, no fingerprint — the semantics
//! live once in `wingfoil_next::ops` and both engines execute that code.

use std::time::Duration;

use wingfoil::codegen::Kernel;
use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::interp::Builder;
use wingfoil_next::op::{Ctx, Op, Tick};
use wingfoil_next::ops::{Filter, Fold, Map, Merge2, Ticker};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(10);

/// Interpreted wiring: ticker → count → even/odd classification → two
/// filtered format branches → merge → accumulate. (Ten nodes.)
fn interpreted_odds_evens(run_for: RunFor) -> Vec<String> {
    let mut g = Builder::new();
    let tick = g.ticker(PERIOD);
    let count = g.fold(tick, 0u64, |acc, _: &()| *acc += 1);
    let is_even = g.map(count, |i: &u64| i.is_multiple_of(2));
    let is_odd = g.map(is_even, |b: &bool| !b);
    let odds = g.filter(count, is_odd);
    let odd_str = g.map(odds, |i: &u64| format!("{i} is odd"));
    let evens = g.filter(count, is_even);
    let even_str = g.map(evens, |i: &u64| format!("{i} is even"));
    let merged = g.merge2(odd_str, even_str);
    let acc = g.fold(merged, Vec::new(), |acc: &mut Vec<String>, v: &String| {
        acc.push(v.clone())
    });
    let mut runner = g.build();
    runner.run(HISTORICAL, run_for);
    runner.value(acc)
}

/// Compiled runner: the same graph, hand-expanded exactly as a `graph!`
/// macro would emit it. Same node order, same `Op` calls, state in locals.
#[allow(clippy::useless_format)]
fn compiled_odds_evens(run_for: RunFor) -> Vec<String> {
    // cfg + state per node (the closures are written once, here — there is
    // no second copy to drift out of sync with the interpreted wiring above
    // once the macro emits both from one definition).
    let mut tick_cfg = NanoTime::from(PERIOD);
    let mut tick_state: Option<NanoTime> = None;
    let mut count_f = |acc: &mut u64, _: &()| *acc += 1;
    let mut count_acc = 0u64;
    let mut even_f = |i: &u64| i.is_multiple_of(2);
    let mut odd_f = |b: &bool| !b;
    let mut odd_str_f = |i: &u64| format!("{i} is odd");
    let mut even_str_f = |i: &u64| format!("{i} is even");
    let mut acc_f = |acc: &mut Vec<String>, v: &String| acc.push(v.clone());
    let mut acc_state: Vec<String> = Vec::new();

    // value slots
    let mut v_count = 0u64;
    let mut v_is_even = false;
    let mut v_is_odd = false;
    let mut v_odds = 0u64;
    let mut v_odd_str = String::new();
    let mut v_evens = 0u64;
    let mut v_even_str = String::new();
    let mut v_merged = String::new();

    let mut k = Kernel::new(HISTORICAL, run_for);
    {
        let mut ctx = Ctx::new(&mut k, 0);
        Ticker::start(&mut tick_cfg, &mut tick_state, &mut ctx);
    }
    let mut dirty = [false; 10];
    while k.begin_cycle(&mut dirty) {
        // [0] ticker — the only op with Caps::SCHEDULES, so the only dirty check.
        let t_tick = dirty[0] && {
            let mut ctx = Ctx::new(&mut k, 0);
            matches!(
                Ticker::cycle(&mut tick_cfg, &mut tick_state, (), &mut ctx),
                Tick::Value(())
            )
        };
        // [1] count
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
        // [2] is_even
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
        // [3] is_odd
        let t_is_odd = t_is_even && {
            let mut ctx = Ctx::new(&mut k, 3);
            match <Map<bool, bool, _>>::cycle(&mut odd_f, &mut (), (&v_is_even,), &mut ctx) {
                Tick::Value(v) => {
                    v_is_odd = v;
                    true
                }
                Tick::Quiet => false,
            }
        };
        // [4] odds = filter(count, is_odd)
        let t_odds = (t_count || t_is_odd) && {
            let mut ctx = Ctx::new(&mut k, 4);
            match <Filter<u64>>::cycle(&mut (), &mut (), (&v_count, &v_is_odd), &mut ctx) {
                Tick::Value(v) => {
                    v_odds = v;
                    true
                }
                Tick::Quiet => false,
            }
        };
        // [5] odd_str
        let t_odd_str = t_odds && {
            let mut ctx = Ctx::new(&mut k, 5);
            match <Map<u64, String, _>>::cycle(&mut odd_str_f, &mut (), (&v_odds,), &mut ctx) {
                Tick::Value(v) => {
                    v_odd_str = v;
                    true
                }
                Tick::Quiet => false,
            }
        };
        // [6] evens = filter(count, is_even)
        let t_evens = (t_count || t_is_even) && {
            let mut ctx = Ctx::new(&mut k, 6);
            match <Filter<u64>>::cycle(&mut (), &mut (), (&v_count, &v_is_even), &mut ctx) {
                Tick::Value(v) => {
                    v_evens = v;
                    true
                }
                Tick::Quiet => false,
            }
        };
        // [7] even_str
        let t_even_str = t_evens && {
            let mut ctx = Ctx::new(&mut k, 7);
            match <Map<u64, String, _>>::cycle(&mut even_str_f, &mut (), (&v_evens,), &mut ctx) {
                Tick::Value(v) => {
                    v_even_str = v;
                    true
                }
                Tick::Quiet => false,
            }
        };
        // [8] merged
        let t_merged = (t_odd_str || t_even_str) && {
            let mut ctx = Ctx::new(&mut k, 8);
            match <Merge2<String>>::cycle(
                &mut (),
                &mut (),
                ((&v_odd_str, t_odd_str), (&v_even_str, t_even_str)),
                &mut ctx,
            ) {
                Tick::Value(v) => {
                    v_merged = v;
                    true
                }
                Tick::Quiet => false,
            }
        };
        // [9] accumulate
        if t_merged {
            let mut ctx = Ctx::new(&mut k, 9);
            let _ = <Fold<String, Vec<String>, _>>::cycle(
                &mut acc_f,
                &mut acc_state,
                (&v_merged,),
                &mut ctx,
            );
        }
        k.end_cycle(&mut dirty);
    }
    acc_state
}

#[test]
fn compiled_matches_interpreted() {
    let run_for = RunFor::Cycles(12);
    let interpreted = interpreted_odds_evens(run_for);
    assert_eq!(12, interpreted.len());
    assert_eq!("1 is odd", interpreted[0]);
    assert_eq!("2 is even", interpreted[1]);
    assert_eq!("12 is even", interpreted[11]);

    let compiled = compiled_odds_evens(run_for);
    assert_eq!(interpreted, compiled);
}

#[test]
fn compiled_matches_interpreted_duration_bound() {
    let run_for = RunFor::Duration(Duration::from_millis(55));
    let interpreted = interpreted_odds_evens(run_for);
    let compiled = compiled_odds_evens(run_for);
    assert!(!interpreted.is_empty());
    assert_eq!(interpreted, compiled);
}

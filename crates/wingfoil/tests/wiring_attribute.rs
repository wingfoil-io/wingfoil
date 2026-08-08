//! **PROTOTYPE: `#[wiring]`** — record every closure with no call-site
//! annotation.
//!
//! The alternative to `func!` + `_q` twins. A user writes completely ordinary
//! wiring; the attribute rewrites every method call carrying a closure into
//! `.<method>(..).__wf_src(<text>, <loc>, <captures>)` and lets method
//! resolution decide what that means: `Stream`'s *inherent* `__wf_src` records,
//! everything else picks up the blanket `MaybeSrc` no-op.
//!
//! **Captures are detected**, so `func!([fee] ..)` is no longer needed to write
//! a capturing closure — free-variable analysis finds `fee` and `Probe` renders
//! it. The two properties that makes usable are pinned below: detection is
//! **soft** (an unrenderable capture refuses at *generation* time, and the
//! wiring still compiles and runs), and it does not **over-detect** (a free
//! function called in a closure body is not a capture).
//!
//! The one remaining cost is pinned too: the recorded text is **normalised**,
//! not verbatim.

use std::time::Duration;

use wingfoil::codegen;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode, wiring};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);

struct Instrument {
    period: Duration,
}

/// Ordinary Rust: no `func!`, no `_q`, no `with_src`, no `with_cfg`. The
/// iterator `.map(..)` and `.reduce(..)` closures are rewritten too — that they
/// still compile and behave is the load-bearing property.
#[wiring]
fn desk(g: &GraphBuilder, cfg: &[Instrument]) -> Stream<f64> {
    let legs: Vec<Stream<f64>> = cfg
        .iter()
        .map(|inst| {
            g.ticker(inst.period)
                .count()
                .map(|n: &u64| *n as f64 * 100.0)
        })
        .collect();

    legs.into_iter()
        .reduce(|a, b| a.join(&b, |x: &f64, y: &f64| x + y))
        .expect("at least one instrument")
}

fn config() -> Vec<Instrument> {
    vec![
        Instrument {
            period: Duration::from_millis(1),
        },
        Instrument {
            period: Duration::from_millis(5),
        },
    ]
}

/// **The point of the whole thing**: unannotated wiring is fully recorded, so
/// the graph is emittable with nothing at the call site.
#[test]
fn unannotated_wiring_is_fully_recorded() {
    let g = GraphBuilder::new();
    let _out = desk(&g, &config());

    let unrecorded: Vec<_> = g
        .describe()
        .into_iter()
        .filter(|n| n.takes_closure_cfg && n.src.is_none())
        .collect();
    assert!(
        unrecorded.is_empty(),
        "every closure node recorded itself: {unrecorded:?}"
    );

    let src = codegen::generate("desk", "f64", |g| desk(g, &config())).expect("emittable");
    assert_eq!(2, src.matches("g.ticker(").count(), "both legs unrolled");
    assert!(!src.contains("for ") && !src.contains(".iter()"), "{src}");
}

/// **The trick that lets it rewrite blindly.** `#[wiring]` cannot tell
/// `Stream::map` from `Iterator::map` — it sees tokens, not types — so it
/// rewrites both. The iterator ones resolve to the blanket `MaybeSrc` no-op and
/// compile to what they did before; only `Stream`'s inherent method records.
///
/// If that precedence ever broke, this test would fail to compile rather than
/// silently mis-record, which is the right failure.
#[test]
fn iterator_closures_are_rewritten_but_inert() {
    let plain = {
        let g = GraphBuilder::new();
        let out = g
            .ticker(Duration::from_millis(1))
            .count()
            .map(|n: &u64| *n as f64 * 100.0)
            .accumulate();
        let mut r = g.build();
        r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
        r.value(out)
    };

    // The same first leg, reached through `iter().map(..).collect()` inside a
    // `#[wiring]` fn — so an iterator closure sits between the caller and the
    // graph.
    let through_iterators = {
        let g = GraphBuilder::new();
        let cfg = [Instrument {
            period: Duration::from_millis(1),
        }];
        let out = desk(&g, &cfg).accumulate();
        let mut r = g.build();
        r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
        r.value(out)
    };

    assert_eq!(vec![100.0, 200.0, 300.0], plain);
    assert_eq!(
        plain, through_iterators,
        "the rewrite must be behaviour-inert"
    );
}

/// **Cost 1, pinned: the text is normalised, not verbatim.** A proc macro
/// cannot recover the original snippet on stable — `Span::source_text` returns
/// only the first token of a multi-token expression, and joining spans is
/// nightly — so the artifact carries `| n : & u64 |` where `func!`'s
/// `stringify!` would give `|n: &u64|`. `rustfmt` does not repair it: it does
/// not format inside macro bodies.
///
/// Asserted rather than merely documented, because it is the whole trade
/// against the `_q` twins and should be impossible to forget.
#[test]
fn recorded_text_is_normalised_not_verbatim() {
    let g = GraphBuilder::new();
    let _out = desk(&g, &config());

    let mapped = g
        .describe()
        .into_iter()
        .find(|n| n.label == "Map")
        .expect("the graph has a map");
    assert_eq!(
        Some("| n : & u64 | * n as f64 * 100.0"),
        mapped.src.as_deref(),
        "spacing is the token stream re-printed, not the source"
    );
}

/// **Captures are detected and frozen, with nothing declared.** This is what
/// `func!([fee] move |p: &f64| p - fee)` produces by hand: the body wrapped in a
/// block that re-materialises the binding, so it resolves wherever it is
/// spliced. Here the attribute found `fee` itself.
#[test]
fn a_capture_is_detected_and_re_materialised() {
    #[wiring]
    fn with_capture(g: &GraphBuilder, fee: f64) -> Stream<f64> {
        g.ticker(Duration::from_millis(1))
            .count()
            .map(|n: &u64| *n as f64)
            .map(move |p: &f64| p - fee)
    }

    let g = GraphBuilder::new();
    let _out = with_capture(&g, 2.5);

    let last = g.describe().pop().expect("nodes");
    assert_eq!(
        Some("{ let fee = 2.5f64; move | p : & f64 | p - fee }"),
        last.src.as_deref(),
        "the capture is bound in the recorded body, so it resolves anywhere"
    );
    assert!(last.unresolved_captures.is_empty());

    // And the whole graph emits, which it could not before.
    let src = codegen::generate("desk", "f64", |g| with_capture(g, 2.5)).expect("emittable");
    assert!(src.contains("let fee = 2.5f64;"), "{src}");
}

/// **The freeze is real, and visible.** Two generations with different wiring
/// arguments produce different artifacts — that is partial evaluation working,
/// and equally how a stale value ships. Pinned so the property is impossible to
/// mistake for a caching bug.
#[test]
fn a_detected_capture_is_frozen_at_generation_time() {
    #[wiring]
    fn priced(g: &GraphBuilder, fee: f64) -> Stream<f64> {
        g.ticker(Duration::from_millis(1))
            .count()
            .map(|n: &u64| *n as f64)
            .map(move |p: &f64| p - fee)
    }

    let a = codegen::generate("desk", "f64", |g| priced(g, 2.5)).expect("emittable");
    let b = codegen::generate("desk", "f64", |g| priced(g, 7.25)).expect("emittable");
    assert!(a.contains("let fee = 2.5f64;"), "{a}");
    assert!(b.contains("let fee = 7.25f64;"), "{b}");
    assert_ne!(a, b);
}

/// **Soft failure is the point of `Probe`.** A closure capturing something with
/// no `EmitLiteral` impl must still *wire and run* — the attribute is applied to
/// a whole function, most of whose nodes are never generated from. Only
/// generation refuses, and it names the binding.
///
/// If detection imposed an `EmitLiteral` bound instead, this function would not
/// compile at all, which would make `#[wiring]` unusable on any real graph.
#[test]
fn an_unrenderable_capture_still_wires_and_runs_but_refuses_to_emit() {
    use std::sync::{Arc, Mutex};

    #[wiring]
    fn with_state(g: &GraphBuilder, book: Arc<Mutex<Vec<u64>>>) -> Stream<u64> {
        g.ticker(Duration::from_millis(1))
            .count()
            .map(move |n: &u64| {
                book.lock().expect("test mutex poisoned").push(*n);
                *n
            })
    }

    // It runs.
    let book = Arc::new(Mutex::new(Vec::new()));
    let g = GraphBuilder::new();
    let out = with_state(&g, Arc::clone(&book)).accumulate();
    let mut r = g.build();
    r.run(HISTORICAL, RunFor::Cycles(3)).unwrap();
    assert_eq!(vec![1, 2, 3], r.value(out));
    assert_eq!(vec![1, 2, 3], *book.lock().unwrap());

    // It refuses to generate, naming the binding rather than saying "not
    // quoted" — which would send a reader to fix something already done.
    let err = codegen::generate("bad", "u64", |g| {
        with_state(g, Arc::new(Mutex::new(Vec::new())))
    })
    .expect_err("an unrenderable capture is a refusal");
    let text = err.to_string();
    assert!(text.contains("`book`"), "{text}");
    assert!(text.contains("EmitLiteral"), "{text}");
    assert!(!text.contains("not quoted"), "wrong advice: {text}");
}

/// **The analysis must not over-detect.** A free function called in a closure
/// body is not a capture: it resolves wherever the artifact is compiled. If it
/// were detected, `Probe` would return `None` for the fn item and this
/// perfectly emittable graph would be refused.
///
/// The cost of that exclusion is stated in `free_vars`: a captured *closure*
/// invoked as `f(x)` is missed. This test pins the trade, not just the case.
#[test]
fn free_functions_and_items_are_not_captures() {
    fn scale(n: &u64) -> f64 {
        *n as f64 * 10.0
    }

    #[wiring]
    fn uses_items(g: &GraphBuilder) -> Stream<f64> {
        g.ticker(Duration::from_millis(1))
            .count()
            .map(|n: &u64| scale(n))
            // `Some`/`None` are variants, `f64::MAX` is a multi-segment path,
            // `TEN` is a const — none of them is a binding to capture.
            .map(|v: &f64| {
                const TEN: f64 = 10.0;
                let bumped = if *v > TEN { Some(*v) } else { None };
                bumped.unwrap_or(f64::MAX)
            })
    }

    let g = GraphBuilder::new();
    let _out = uses_items(&g);
    for n in g.describe() {
        assert!(
            n.unresolved_captures.is_empty(),
            "node {} over-detected {:?}",
            n.index,
            n.unresolved_captures
        );
    }
    codegen::generate("items", "f64", uses_items).expect("emittable");
}

/// **Locals bound inside the closure are not captures either** — the other half
/// of not over-detecting. Every pattern form the flat `Bound` collector has to
/// cover appears here, so a regression in any one of them shows up as a refusal
/// rather than silently.
#[test]
fn names_bound_inside_the_closure_are_not_captures() {
    #[wiring]
    fn binds(g: &GraphBuilder) -> Stream<u64> {
        g.ticker(Duration::from_millis(1)).count().map(|n: &u64| {
            let doubled = n * 2; // let
            let mut total = 0u64;
            for step in 0..doubled {
                // for pattern
                total += step;
            }
            let paired = (total, doubled);
            let (lhs, rhs) = paired; // tuple pattern
            match Some(lhs) {
                Some(hit) => hit + rhs, // match arm binding
                None => rhs,
            }
        })
    }

    let g = GraphBuilder::new();
    let _out = binds(&g);
    for n in g.describe() {
        assert!(
            n.unresolved_captures.is_empty(),
            "node {} over-detected {:?}",
            n.index,
            n.unresolved_captures
        );
    }
}

// ---------------------------------------------------------------------------
// The end-to-end proof: a per-instrument capture, discovered by running a loop,
// baked into the pipeline that loop wired.
//
// The artifact sits below as a real `nitro!` block, so "the generated text is
// valid Rust" is proven by this file compiling rather than asserted. Same
// device as `tests/codegen_emission.rs`.
// ---------------------------------------------------------------------------

struct Leg {
    period: Duration,
    fee: f64,
}

/// Pass 1. Note what is *not* here: no `func!`, no `_q`, no `with_src`, no
/// `with_cfg`, and no capture declaration — `fee` is found by analysis.
#[wiring]
fn book(g: &GraphBuilder, legs: &[Leg]) -> Stream<f64> {
    legs.iter()
        .map(|leg| {
            let fee = leg.fee;
            g.ticker(leg.period)
                .count()
                .map(move |n: &u64| *n as f64 - fee)
        })
        .reduce(|a, b| a.join(&b, |x: &f64, y: &f64| x + y))
        .expect("at least one leg")
}

fn legs() -> [Leg; 2] {
    [
        Leg {
            period: Duration::from_millis(1),
            fee: 0.5,
        },
        Leg {
            period: Duration::from_millis(5),
            fee: 1.25,
        },
    ]
}

// Pass 2's input, verbatim. The two `let fee = ..;` blocks are the detected
// captures re-materialised — each leg's own value, frozen.
wingfoil::nitro! {
    fn book_generated(g: &GraphBuilder) -> Stream<f64> {
        let n0 = g.ticker(::core::time::Duration::new(0u64, 1000000u32));
        let n1 = n0.count();
        let n2 = n1.map({ let fee = 0.5f64; move | n : & u64 | * n as f64 - fee });
        let n3 = g.ticker(::core::time::Duration::new(0u64, 5000000u32));
        let n4 = n3.count();
        let n5 = n4.map({ let fee = 1.25f64; move | n : & u64 | * n as f64 - fee });
        let n6 = n2.join(&n5, | x : & f64, y : & f64 | x + y);
        n6
    }
}

/// The walker's output must be **exactly** the block above. String match plus
/// this file compiling proves the artifact is valid `nitro!` input by
/// construction — the loop is gone, unrolled into two pipelines, and each
/// carries its own captured fee.
#[test]
fn a_per_instrument_capture_emits_a_valid_artifact() {
    let src = codegen::generate("book_generated", "f64", |g| book(g, &legs()))
        .expect("emittable with nothing annotated");

    let expected = concat!(
        "wingfoil::nitro! {\n",
        "    fn book_generated(g: &GraphBuilder) -> Stream<f64> {\n",
        "        let n0 = g.ticker(::core::time::Duration::new(0u64, 1000000u32));\n",
        "        let n1 = n0.count();\n",
        "        let n2 = n1.map({ let fee = 0.5f64; move | n : & u64 | * n as f64 - fee });\n",
        "        let n3 = g.ticker(::core::time::Duration::new(0u64, 5000000u32));\n",
        "        let n4 = n3.count();\n",
        "        let n5 = n4.map({ let fee = 1.25f64; move | n : & u64 | * n as f64 - fee });\n",
        "        let n6 = n2.join(&n5, | x : & f64, y : & f64 | x + y);\n",
        "        n6\n",
        "    }\n",
        "}",
    );
    assert_eq!(expected, src);
}

/// **Parity**: the generated graph computes what the wiring it came from
/// computes, on values *and* tick times. Asserted across all three tiers, since
/// a capture spliced into a block is exactly the sort of thing that could
/// compile and mean something different.
#[test]
fn the_generated_graph_matches_the_wiring_it_came_from() {
    let interpreted = {
        let g = GraphBuilder::new();
        let out = book(&g, &legs()).with_time().accumulate();
        let mut r = g.build();
        r.run(HISTORICAL, RunFor::Cycles(8)).unwrap();
        r.value(out)
    };

    // `interpreted()` hands back a built `Runner` and a `Handle`, so there is
    // nothing left to wire `with_time` onto. Mounting the artifact as an island
    // gives a `Stream` again, and tick times with it.
    let generated = {
        let g = GraphBuilder::new();
        let out = book_generated::nested(&g).with_time().accumulate();
        let mut r = g.build();
        r.run(HISTORICAL, RunFor::Cycles(8)).unwrap();
        r.value(out)
    };

    assert!(!interpreted.is_empty(), "the fixture must actually tick");
    assert_eq!(interpreted, generated, "values and tick times must agree");

    // And the compiled tier, which is the point of generating at all.
    let (compiled,) = book_generated::compiled(HISTORICAL, RunFor::Cycles(8)).unwrap();
    assert_eq!(
        interpreted.last().map(|(_, v)| *v),
        Some(compiled),
        "compiled tier agrees with the wiring"
    );
}

//! **PROTOTYPE: `#[wiring]`** — record every closure with no call-site
//! annotation.
//!
//! The alternative to `func!` + `_q` twins. A user writes completely ordinary
//! wiring; the attribute rewrites every method call carrying a closure into
//! `.<method>(..).__wf_src(<text>, <loc>)` and lets method resolution decide
//! what that means: `Stream`'s *inherent* `__wf_src` records, everything else
//! picks up the blanket `MaybeSrc` no-op.
//!
//! Two costs, both pinned below so the trade is visible rather than described:
//! the recorded text is **normalised**, not verbatim, and captures are not
//! detected.

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

/// **Cost 2, pinned: captures are not detected.** `func!([fee] ..)` records the
/// captured *value* and re-materialises it; this records a body referencing a
/// name that exists only in the wiring. The node looks emittable and the
/// artifact then fails at pass 2 — a worse failure than a refusal, and the gap
/// that would have to close before this could replace `func!`.
#[test]
fn a_capture_is_recorded_but_unresolvable() {
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
        Some("move | p : & f64 | p - fee"),
        last.src.as_deref(),
        "the body is recorded verbatim-ish — but `fee` is not in it"
    );
    // Emission accepts it, and the artifact would not compile.
    assert!(
        codegen::generate("bad", "f64", |g| with_capture(g, 2.5)).is_ok(),
        "the gap: an undeclared capture passes the eligibility check"
    );
}

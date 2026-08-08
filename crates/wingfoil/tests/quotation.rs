//! **Closure quotation and graph introspection** (#726 step 2).
//!
//! A wired graph erases its closures — the interpreted engine holds each node's
//! cycle as a `Box<dyn FnMut>` over `Rc<dyn Any>` slots — so traversal recovers
//! topology but never what a node *computes*. `func!` keeps the closure's
//! tokens at the call site, `Stream::with_src` records them against the node,
//! and `describe()` reads them back.
//!
//! The property that makes this trustworthy: the text and the closure come from
//! the **same tokens**, so what a reader sees is what ran. Drift is impossible
//! by construction rather than by discipline — which is the failure mode that
//! sank the deleted legacy `codegen` retrofit, where a human re-stated each
//! closure by hand.

use std::time::Duration;

use wingfoil::func;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(1);
const RUN: RunFor = RunFor::Cycles(5);

// ---------------------------------------------------------------------------
// The quotation itself.
// ---------------------------------------------------------------------------

/// `stringify!` on an `$f:expr` metavariable returns the **verbatim** source
/// snippet, spacing intact — not the token stream re-printed as
/// `| x : & f64 | x * 2.0`. That is what lets a generated artifact read like
/// hand-written code (decision doc §5).
#[test]
fn func_captures_verbatim_source_and_location() {
    let q = func!(|x: &f64| x * 2.0);
    assert_eq!("|x: &f64| x * 2.0", q.src);
    assert_eq!(8.0, (q.f)(&4.0), "the closure still runs");
    assert!(q.loc.0.ends_with("quotation.rs"), "file was {}", q.loc.0);
    assert!(q.loc.1 > 0);
}

/// The no-drift property, stated as a test: the recorded text, re-spliced as a
/// closure literal at a *different* call site, computes what the original did.
#[test]
fn recorded_source_reparses_to_the_behaviour_that_ran() {
    let q = func!(|x: &f64| x * 2.0);
    // What a generator would splice into its artifact — the same tokens.
    let respliced = |x: &f64| x * 2.0;
    assert_eq!("|x: &f64| x * 2.0", q.src);
    for probe in [0.0f64, 1.5, -3.25, 1e9] {
        assert_eq!(
            (q.f)(&probe),
            respliced(&probe),
            "quoted and re-spliced forms diverged at {probe}"
        );
    }
}

/// `func!` is arity-agnostic: it captures the closure as one expression rather
/// than destructuring its parameter list, so it covers every shape the catalog
/// takes — `map`'s `Fn(&A) -> B`, `join`'s `Fn(&A, &B) -> C`, `fold`'s
/// `Fn(&mut S, &A)`. (The decision doc's fn-pointer coercion could not: it has
/// to name an arity.)
#[test]
fn func_works_at_every_arity_the_catalog_uses() {
    let unary = func!(|a: &u64| a + 1);
    let binary = func!(|a: &u64, b: &u64| a + b);
    let ternary = func!(|a: &u64, b: &u64, c: &u64| a + b + c);
    let acc = func!(|acc: &mut u64, v: &u64| *acc += v);

    assert_eq!(2, (unary.f)(&1));
    assert_eq!(5, (binary.f)(&2, &3));
    assert_eq!(9, (ternary.f)(&2, &3, &4));
    let mut total = 10u64;
    (acc.f)(&mut total, &5);
    assert_eq!(15, total);

    assert_eq!("|a: &u64, b: &u64| a + b", binary.src);
    assert_eq!("|acc: &mut u64, v: &u64| *acc += v", acc.src);
}

// ---------------------------------------------------------------------------
// Attaching it to a graph.
// ---------------------------------------------------------------------------

#[test]
fn with_src_records_against_the_node_and_reads_back() {
    let g = GraphBuilder::new();
    let ticks = g.ticker(PERIOD).count();

    let double = func!(|i: &u64| i * 2);
    let doubled = ticks.map(double.f).with_src(&double);

    assert_eq!(Some("|i: &u64| i * 2"), doubled.src().as_deref());
    // An unannotated node reports nothing — `None` means "not quoted", not
    // "no closure": the engine erased it and no traversal can get it back.
    assert_eq!(None, ticks.src());
}

/// One method covers the whole catalog, because it annotates the *node* a
/// stream refers to rather than intercepting a particular op's config. Here:
/// three different ops, three arities, one mechanism.
#[test]
fn with_src_covers_ops_of_different_arities() {
    let g = GraphBuilder::new();
    let ticks = g.ticker(PERIOD).count();

    let scale = func!(|i: &u64| i * 10);
    let scaled = ticks.map(scale.f).with_src(&scale);

    let combine = func!(|a: &u64, b: &u64| a + b);
    let joined = ticks.join(&scaled, combine.f).with_src(&combine);

    let total = func!(|acc: &mut u64, v: &u64| *acc += v);
    let folded = joined.fold(0u64, total.f).with_src(&total);

    assert_eq!(Some("|i: &u64| i * 10"), scaled.src().as_deref());
    assert_eq!(Some("|a: &u64, b: &u64| a + b"), joined.src().as_deref());
    assert_eq!(
        Some("|acc: &mut u64, v: &u64| *acc += v"),
        folded.src().as_deref()
    );
}

/// Quotation is wiring-time metadata only — it must not change what the graph
/// computes. The same graph wired both ways produces identical values.
#[test]
fn quoting_does_not_change_behaviour() {
    let plain = {
        let g = GraphBuilder::new();
        let out = g.ticker(PERIOD).count().map(|i: &u64| i * 3).accumulate();
        let mut runner = g.build();
        runner.run(HISTORICAL, RUN).unwrap();
        runner.value(out)
    };

    let quoted = {
        let g = GraphBuilder::new();
        let triple = func!(|i: &u64| i * 3);
        let out = g
            .ticker(PERIOD)
            .count()
            .map(triple.f)
            .with_src(&triple)
            .accumulate();
        let mut runner = g.build();
        runner.run(HISTORICAL, RUN).unwrap();
        runner.value(out)
    };

    assert_eq!(vec![3u64, 6, 9, 12, 15], plain);
    assert_eq!(plain, quoted, "quotation must be inert at runtime");
}

// ---------------------------------------------------------------------------
// The introspection surface this exists to make useful.
// ---------------------------------------------------------------------------

/// `describe()` is the graph's own account of itself: op kinds, edges,
/// activations, and — where quoted — what each node computes. This is step 2's
/// independent value, whether or not the generator of step 3 is ever built.
#[test]
fn describe_reports_topology_and_sources() {
    let g = GraphBuilder::new();
    let ticks = g.ticker(PERIOD).count();
    let double = func!(|i: &u64| i * 2);
    let _doubled = ticks.map(double.f).with_src(&double);

    let runner = g.build();
    let nodes = runner.describe();

    assert_eq!(3, nodes.len(), "ticker + count + map");
    assert_eq!(0, nodes[0].index);
    // The label is the op's *type* name (shortened `type_name`), not its
    // `#[op(build = ..)]` method name — `Ticker`, not `ticker`.
    assert_eq!("Ticker", nodes[0].label);
    assert!(
        nodes[0].active_ups.is_empty(),
        "a source has no upstream edges"
    );
    assert!(
        nodes[0].activation.schedules,
        "a ticker re-arms its own callbacks"
    );

    let mapped = nodes.last().expect("three nodes");
    assert_eq!(vec![1usize], mapped.active_ups, "map reads count");
    assert_eq!(Some("|i: &u64| i * 2"), mapped.src.as_deref());
    assert_eq!(
        Some(true),
        mapped.loc.map(|(f, _)| f.ends_with("quotation.rs"))
    );
}

/// The builder-side twin, for inspecting a graph while it is still being
/// assembled — which is where a generator's pass 1 would read from.
#[test]
fn describe_works_before_build() {
    let g = GraphBuilder::new();
    let ticks = g.ticker(PERIOD).count();
    assert_eq!(2, g.describe().len());

    let double = func!(|i: &u64| i * 2);
    let _doubled = ticks.map(double.f).with_src(&double);

    let nodes = g.describe();
    assert_eq!(3, nodes.len());
    assert_eq!(Some("|i: &u64| i * 2"), nodes[2].src.as_deref());
}

/// The shape a generator's pass 1 actually consumes: wiring driven by *data*,
/// unrolled into per-item pipelines, each carrying its own quoted source. This
/// is the topology `nitro!` structurally cannot express — the whole reason the
/// two-pass design exists.
#[test]
fn a_procedurally_wired_graph_describes_every_generated_node() {
    let factors: [u64; 3] = [2, 5, 11];

    let g = GraphBuilder::new();
    let ticks = g.ticker(PERIOD).count();
    // One quotation, reused across the loop — the closure is closed, so every
    // branch computes the same thing over a different input.
    let dbl = func!(|i: &u64| i * 2);
    for f in factors {
        let scaled = ticks.map(move |i: &u64| i * f);
        let _ = scaled.map(dbl.f).with_src(&dbl);
    }

    let runner = g.build();
    let nodes = runner.describe();

    // ticker + count + (scale, double) per factor.
    assert_eq!(2 + 2 * factors.len(), nodes.len());

    let quoted: Vec<_> = nodes.iter().filter_map(|n| n.src.as_deref()).collect();
    assert_eq!(
        vec!["|i: &u64| i * 2"; factors.len()],
        quoted,
        "every generated branch reports its source"
    );

    // The un-quoted `move |i| i * f` closures are exactly the ones a generator
    // would have to reject: they capture, and the engine erased them.
    let unquoted = nodes
        .iter()
        .filter(|n| n.label.contains("Map") && n.src.is_none())
        .count();
    assert_eq!(
        factors.len(),
        unquoted,
        "capturing closures stay invisible to traversal"
    );
}

// ---------------------------------------------------------------------------
// Tier 2: explicit capture lists.
// ---------------------------------------------------------------------------

/// A **closed** closure's body resolves anywhere. A capturing one does not:
/// `move |p| p * fee` refers to a binding that exists only where it was
/// written. Declaring the capture records its *value*, so the body can be
/// spliced inside a block that re-materialises it.
#[test]
fn a_declared_capture_is_recorded_by_value() {
    let fee = 2.5f64;
    let q = func!([fee] move |p: &f64| p * fee);

    assert_eq!(7.5, (q.f)(&3.0), "the closure still runs, unchanged");
    assert_eq!(1, q.captures.len());
    assert_eq!("fee", q.captures[0].name);
    assert_eq!("2.5f64", q.captures[0].value);
    assert_eq!(
        "{ let fee = 2.5f64; move |p: &f64| p * fee }",
        q.emittable_src()
    );
}

/// Several captures, and the emitted block binds them all in declaration order.
#[test]
fn multiple_captures_are_all_bound() {
    let lo = 1u64;
    let hi = 9u64;
    let q = func!([lo, hi] move |v: &u64| v.clamp(&lo, &hi).to_owned());

    assert_eq!(9, (q.f)(&100));
    assert_eq!(1, (q.f)(&0));
    assert_eq!(
        "{ let lo = 1u64; let hi = 9u64; move |v: &u64| v.clamp(&lo, &hi).to_owned() }",
        q.emittable_src()
    );
}

/// The re-materialised block computes what the original closure did — the same
/// no-drift property tier 1 has, extended over the captures. Written out as
/// real source, so the file compiling proves the block is valid Rust.
#[test]
fn a_rematerialised_capture_computes_what_the_original_did() {
    let fee = 2.5f64;
    let q = func!([fee] move |p: &f64| p * fee);

    // Exactly what `emittable_src` produces, spliced at a different call site
    // where `fee` is *not* in scope as the same binding.
    let respliced = {
        let fee = 2.5f64;
        move |p: &f64| p * fee
    };
    assert_eq!(
        "{ let fee = 2.5f64; move |p: &f64| p * fee }",
        q.emittable_src()
    );
    for probe in [0.0f64, 1.5, -3.25, 1e6] {
        assert_eq!((q.f)(&probe), respliced(&probe), "diverged at {probe}");
    }
}

/// A tier-1 quotation records no captures and emits its body bare, so the
/// common case pays nothing for tier 2 existing.
#[test]
fn a_closed_closure_records_no_captures() {
    let q = func!(|p: &f64| p * 2.0);
    assert!(q.captures.is_empty());
    assert_eq!("|p: &f64| p * 2.0", q.emittable_src());
}

/// Captures reach the node, so a graph reports the emittable form — not the
/// bare body, which would not resolve where the artifact is compiled.
#[test]
fn with_src_records_the_emittable_form_of_a_capture() {
    let g = GraphBuilder::new();
    let fee = 0.25f64;
    let adjust = func!([fee] move |p: &f64| p - fee);
    let px = g
        .ticker(PERIOD)
        .count()
        .map(|i: &u64| *i as f64)
        .map(adjust.f)
        .with_src(&adjust);

    assert_eq!(
        Some("{ let fee = 0.25f64; move |p: &f64| p - fee }"),
        px.src().as_deref()
    );
}

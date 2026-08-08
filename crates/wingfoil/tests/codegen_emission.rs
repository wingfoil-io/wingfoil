//! **`codegen`: emitting `nitro!` wiring source from a wired graph** (#726).
//!
//! Pass 1 runs an ordinary wiring function; the walker prints the graph it
//! built as `nitro!` input; pass 2 is an ordinary build. This file covers the
//! middle step, plus the parity obligation that makes it trustworthy.
//!
//! # How pass 2 is proven without invoking a second compiler
//!
//! A generator's output is a file a later `cargo build` compiles, and a test
//! cannot easily run rustc on generated text. So each intended artifact sits in
//! *this file* as a real `nitro!` block, and the walker must emit **exactly**
//! that text. Strings match + the file compiles => the emitted wiring is valid
//! by construction, not merely plausible. Parity then runs against the graph it
//! was generated from, on values *and* tick times.
//!
//! # Coverage
//!
//! Emission handles single-edge chains, multi-edge ops (`join`), passive-edge
//! ops (`sample`, whose call order the partitioned active/passive lists cannot
//! express without the recorded `passive_mask`), data configs via `EmitLiteral`,
//! and quoted closures. A config-driven loop emits as N unrolled pipelines with
//! no loop surviving — the topology `nitro!` structurally cannot express, and
//! the whole reason the two-pass design exists.
//!
//! Refused, loudly and by test: variadic ops (`merge_all`, `combine`), whose
//! hand-written forwarders carry no `build` name, and erased closures the
//! wiring did not quote. Refusals name node indices rather than the *call
//! sites* `#[track_caller]` would give — the remaining gap.

use std::time::Duration;

use wingfoil::codegen::{self, Breadcrumbs};
use wingfoil::func;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// The walker under test, with this file's fixed emission options.
fn emit(nodes: &[wingfoil::interp::NodeInfo], fn_name: &str, out_ty: &str) -> String {
    codegen::emit(nodes, fn_name, out_ty, Breadcrumbs::Off).expect("graph should be emittable")
}

/// The refusal path: every reason the graph could not be emitted.
fn refusals(nodes: &[wingfoil::interp::NodeInfo]) -> Vec<codegen::Ineligible> {
    codegen::ineligible(nodes)
}

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(1);
const RUN: RunFor = RunFor::Cycles(5);

// ---------------------------------------------------------------------------
// The walker.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The artifact the walker must produce — a real `nitro!` block, so "the emitted
// text is valid Rust" is proven by this file compiling.
// ---------------------------------------------------------------------------

wingfoil::nitro! {
    fn target(g: &GraphBuilder) -> Stream<u64> {
        let n0 = g.ticker(::core::time::Duration::new(0u64, 1000000u32));
        let n1 = n0.count();
        let n2 = n1.map(|i: &u64| i * 2);
        n2
    }
}

wingfoil::nitro! {
    fn joined_target(g: &GraphBuilder) -> Stream<u64> {
        let n0 = g.ticker(::core::time::Duration::new(0u64, 1000000u32));
        let n1 = n0.count();
        let n2 = n1.map(|i: &u64| i * 10);
        let n3 = n1.join(&n2, |x: &u64, y: &u64| x + y);
        n3
    }
}

/// The same graph, wired procedurally — what a generator's pass 1 consumes.
fn wire_source_graph() -> (GraphBuilder, Stream<u64>) {
    let g = GraphBuilder::new();
    let ticks = g.ticker(PERIOD).count();
    let double = func!(|i: &u64| i * 2);
    let doubled = ticks.map(double.f).with_src(&double);
    (g, doubled)
}

// ---------------------------------------------------------------------------
// What works.
// ---------------------------------------------------------------------------

#[test]
fn the_walker_emits_the_expected_wiring_source() {
    let (g, _out) = wire_source_graph();
    let emitted = emit(&g.describe(), "target", "u64");

    // Byte-identical to the `nitro!` block above, which compiles — so the
    // emitted text is valid wiring source, not merely plausible-looking.
    let expected = "\
wingfoil::nitro! {
    fn target(g: &GraphBuilder) -> Stream<u64> {
        let n0 = g.ticker(::core::time::Duration::new(0u64, 1000000u32));
        let n1 = n0.count();
        let n2 = n1.map(|i: &u64| i * 2);
        n2
    }
}";
    assert_eq!(expected, emitted);
}

/// The parity obligation: the artifact agrees with the graph it was generated
/// from — values **and** tick times.
#[test]
fn emitted_graph_matches_the_interpreted_one() {
    let (g, out) = wire_source_graph();
    let acc = out.with_time().accumulate();
    let mut runner = g.build();
    runner.run(HISTORICAL, RUN).unwrap();
    let source_graph = runner.value(acc);

    // `interpreted()` hands back a built `Runner` and a `Handle`, so there is
    // nothing left to wire `with_time` onto. Mounting the artifact as an island
    // gives a `Stream` again, and tick times with it.
    let g2 = GraphBuilder::new();
    let island = target::nested(&g2);
    let acc2 = island.with_time().accumulate();
    let mut runner2 = g2.build();
    runner2.run(HISTORICAL, RUN).unwrap();
    let generated = runner2.value(acc2);

    assert_eq!(
        vec![
            (NanoTime::ZERO, 2u64),
            (NanoTime::from(1_000_000u64), 4),
            (NanoTime::from(2_000_000u64), 6),
            (NanoTime::from(3_000_000u64), 8),
            (NanoTime::from(4_000_000u64), 10),
        ],
        source_graph
    );
    assert_eq!(
        source_graph, generated,
        "the generated artifact must agree with the graph it came from"
    );

    // And the compiled tier, which is the entire point of generating: same
    // values, no interpreter.
    let (compiled,) = target::compiled(HISTORICAL, RUN).unwrap();
    assert_eq!(10, compiled, "compiled artifact's final value");
}

// ---------------------------------------------------------------------------
// The gaps — each pinned so it cannot regress into a silent partial emission.
// ---------------------------------------------------------------------------

/// **Closed (was gap 1): data configs are now recoverable.** `EmitLiteral`
/// renders the value and `with_cfg` records it, so the artifact carries
/// `Duration::new(0, 1_000_000)` rather than depending on a `PERIOD` constant
/// being in scope wherever it is compiled. That self-containment is the point:
/// a generated file is compiled in a scope the generator does not control.
///
/// The cost, per §3: this **freezes** the value. Regenerating is the only way
/// to change it.
#[test]
fn data_configs_are_recorded_and_self_contained() {
    let (g, _out) = wire_source_graph();
    let nodes = g.describe();
    assert_eq!(Some("ticker"), nodes[0].build);
    assert_eq!(
        Some("::core::time::Duration::new(0u64, 1000000u32)"),
        nodes[0].cfg_src.as_deref()
    );

    let emitted = emit(&nodes, "target", "u64");
    assert!(
        !emitted.contains("PERIOD"),
        "the artifact must not depend on the generator's constants:\n{emitted}"
    );
}

/// **Closed (was gap 2): an unquoted closure is now distinguishable from no
/// closure at all.** `#[op]` knows whether an op's `Cfg` is a closure — it can
/// see the `Fn` bound on the config type parameter — and records it, so
/// `takes_closure_cfg && src.is_none()` states exactly "this node has a closure
/// the engine erased and the wiring did not quote".
///
/// The walker refuses such a node instead of printing `map()`, which is the
/// "loud, precise failure" §4.1 asks for. What it still lacks is the *call
/// site*: `#[track_caller]` on the wiring methods would name the line the user
/// wrote, rather than the node index.
#[test]
fn an_erased_closure_is_refused_not_silently_emitted() {
    let g = GraphBuilder::new();
    let factor = 3u64;
    let _out = g
        .ticker(PERIOD)
        .count()
        // Captures `factor` — erased, unrecoverable.
        .map(move |i: &u64| i * factor);

    let nodes = g.describe();
    assert!(!nodes[1].takes_closure_cfg, "count has no closure config");
    assert!(nodes[2].takes_closure_cfg, "map does");
    assert_eq!(None, nodes[2].src.as_deref(), "and it was not quoted");

    let err = refusals(&nodes);
    assert_eq!(1, err.len(), "only the map is ineligible: {err:?}");
    assert_eq!(2, err[0].index);
    assert!(err[0].reason.contains("map_q"), "{:?}", err[0]);
}

/// **Regression: a seeded accumulator whose seed was never recorded must be
/// refused, not emitted without it.**
///
/// `Fold`'s `Cfg` *is* its closure — the seed arrives via `#[op(init_arg)]`, a
/// separate call-site argument with no other trace on the node. So quoting the
/// closure satisfies `takes_closure_cfg` while saying nothing about the seed,
/// and the walker used to print `.fold(f)`: an artifact silently missing the
/// accumulator's starting value, reported as fully emittable.
///
/// That is the exact failure the module docs promise cannot happen ("a refusal,
/// never a partial artifact"), so the flag is recorded and checked. Pass 2 would
/// have caught it on arity — but in generated code, which is the wrong place.
#[test]
fn a_seeded_accumulator_without_its_seed_is_refused() {
    let g = GraphBuilder::new();
    let _out = g
        .ticker(PERIOD)
        .count()
        .fold_q(0u64, func!(|acc: &mut u64, v: &u64| *acc += v));

    let nodes = g.describe();
    let fold = nodes.last().expect("nodes");
    assert!(fold.has_init_arg, "fold takes a call-site seed");
    assert!(fold.src.is_some(), "and its closure *was* recorded");
    assert_eq!(None, fold.cfg_src.as_deref(), "but the seed was not");

    let err = refusals(&nodes);
    assert_eq!(1, err.len(), "{err:?}");
    assert!(err[0].reason.contains("with_cfg"), "{:?}", err[0]);
    assert!(err[0].reason.contains("seed"), "{:?}", err[0]);
}

/// The other half: recording the seed with `.with_cfg(&seed)` emits it in the
/// right position — data config first, then the closure, matching every such
/// signature in the catalog.
#[test]
fn a_recorded_seed_is_emitted_before_the_closure() {
    let g = GraphBuilder::new();
    const SEED: u64 = 7;
    let _out = g
        .ticker(PERIOD)
        .count()
        .fold_q(SEED, func!(|acc: &mut u64, v: &u64| *acc += v))
        .with_cfg(&SEED);

    let nodes = g.describe();
    assert!(refusals(&nodes).is_empty(), "{:?}", refusals(&nodes));
    let emitted = emit(&nodes, "seeded", "u64");
    assert!(
        emitted.contains("fold(7u64, |acc: &mut u64, v: &u64| *acc += v)"),
        "seed first, then the closure:\n{emitted}"
    );
}

/// The other half of the same property: a config-free op is **not** flagged, so
/// refusing an erased closure does not also refuse every `count` in the graph.
#[test]
fn config_free_ops_are_not_mistaken_for_erased_closures() {
    let (g, _out) = wire_source_graph();
    let nodes = g.describe();
    assert!(!nodes[0].takes_closure_cfg, "ticker's Cfg is a Duration");
    assert!(!nodes[1].takes_closure_cfg, "count takes no config");
    assert!(nodes[2].takes_closure_cfg, "map takes a closure");

    let _ = emit(&nodes, "target", "u64");
}

/// **Closed (was gap 3): multi-edge ops emit.** `active_ups` keeps
/// receiver-first order, so a `join` reconstructs as `a.join(&b, f)` — the
/// receiver, then each `&stream` argument, then the config.
#[test]
fn a_multi_edge_graph_emits_and_matches() {
    let g = GraphBuilder::new();
    let a = g.ticker(PERIOD).count();
    let scale = func!(|i: &u64| i * 10);
    let b = a.map(scale.f).with_src(&scale);
    let combine = func!(|x: &u64, y: &u64| x + y);
    let joined = a.join(&b, combine.f).with_src(&combine);

    let emitted = emit(&g.describe(), "joined_target", "u64");
    let expected = "\
wingfoil::nitro! {
    fn joined_target(g: &GraphBuilder) -> Stream<u64> {
        let n0 = g.ticker(::core::time::Duration::new(0u64, 1000000u32));
        let n1 = n0.count();
        let n2 = n1.map(|i: &u64| i * 10);
        let n3 = n1.join(&n2, |x: &u64, y: &u64| x + y);
        n3
    }
}";
    assert_eq!(expected, emitted);

    // And it computes the same thing, values and tick times.
    let acc = joined.with_time().accumulate();
    let mut runner = g.build();
    runner.run(HISTORICAL, RUN).unwrap();
    let source_graph = runner.value(acc);

    let g2 = GraphBuilder::new();
    let acc2 = joined_target::nested(&g2).with_time().accumulate();
    let mut runner2 = g2.build();
    runner2.run(HISTORICAL, RUN).unwrap();

    assert_eq!(source_graph, runner2.value(acc2));
}

/// **The passive case, which the partitioned lists genuinely could not
/// express.** `sample`'s data leg is edge 0 and *passive*; its trigger is edge
/// 1 and active. So `active_ups = [ticker]` and `passive_ups = [count]`, and
/// nothing in that pair says the call was `count.sample(&ticker)` rather than
/// the reverse — both orderings produce the identical pair. The recorded
/// `passive_mask` is what supplies the missing bit.
#[test]
fn passive_edges_reconstruct_the_original_call_order() {
    let g = GraphBuilder::new();
    let tick = g.ticker(PERIOD);
    let count = tick.count();
    let _sampled = count.sample(&tick);

    let nodes = g.describe();
    let sample = &nodes[2];
    assert_eq!(1, sample.passive_mask, "sample is `passive = [0]`");
    assert_eq!(vec![0usize], sample.active_ups, "the trigger");
    assert_eq!(vec![1usize], sample.passive_ups, "the data leg");
    assert_eq!(
        vec![1usize, 0],
        sample.edges_in_call_order(),
        "data first, trigger second — the order `count.sample(&tick)` was written in"
    );

    let emitted = emit(&nodes, "sampled", "u64");
    assert!(
        emitted.contains("let n2 = n1.sample(&n0);"),
        "receiver and argument the wrong way round:\n{emitted}"
    );
}

/// A variadic op (`merge_all` -> `MergeN`) has hand-written forwarders and so
/// no `#[op(build = ..)]` name. It is refused rather than emitted as an
/// n-ary call it would not accept — the remaining loud failure, and the right
/// default for a shape the walker cannot express.
#[test]
fn variadic_ops_are_still_refused() {
    let g = GraphBuilder::new();
    let a = g.ticker(PERIOD).count();
    let scale = func!(|i: &u64| i * 10);
    let b = a.map(scale.f).with_src(&scale);
    let other = func!(|i: &u64| i * 100);
    let c = a.map(other.f).with_src(&other);
    let _merged = a.merge_all(&[&b, &c]);

    let err = refusals(&g.describe());
    assert!(
        err.iter().any(|e| e.reason.contains("hand-written")),
        "{err:?}"
    );
    // And the emitter itself refuses, rather than printing something broken.
    assert!(codegen::emit(&g.describe(), "merged", "u64", Breadcrumbs::Off).is_err());
}

/// The unrolling property, which is the whole point: pass 1 *ran the loop*, so
/// a data-driven topology emits as N explicit pipelines. `nitro!` cannot
/// express the loop; it does not have to, because it never sees one.
#[test]
fn a_config_driven_loop_emits_as_unrolled_pipelines() {
    let g = GraphBuilder::new();
    let ticks = g.ticker(PERIOD).count();
    let double = func!(|i: &u64| i * 2);
    for _ in 0..3 {
        let _ = ticks.map(double.f).with_src(&double);
    }

    let emitted = emit(&g.describe(), "unrolled", "u64");
    assert_eq!(
        3,
        emitted.matches("|i: &u64| i * 2").count(),
        "one pipeline per loop iteration:\n{emitted}"
    );
    assert!(
        !emitted.contains("for "),
        "no loop survives into the artifact"
    );
}

// ---------------------------------------------------------------------------
// The end-to-end entry point: run the wiring, emit, write the artifact.
// ---------------------------------------------------------------------------

/// A unique temp path per test process, per the house rule that parallel tests
/// must never collide on a filename.
fn temp_artifact_path(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "wingfoil_codegen_{tag}_{}_{n}.gen.rs",
        std::process::id()
    ))
}

/// `generate` runs pass 1 itself, so the caller writes a wiring *function* and
/// never touches a builder description. This is the shape the decision doc's
/// §5 example uses.
#[test]
fn generate_runs_the_wiring_and_emits_from_it() {
    let periods = [Duration::from_millis(1), Duration::from_millis(2)];

    let src = codegen::generate("desk", "u64", |g| {
        let double = func!(|i: &u64| i * 2);
        let mut last = None;
        for p in periods {
            last = Some(g.ticker(p).count().map(double.f).with_src(&double));
        }
        last.expect("at least one period")
    })
    .expect("graph should be emittable");

    // The loop ran in pass 1, so the artifact carries both periods unrolled and
    // no loop at all.
    assert!(src.contains("Duration::new(0u64, 1000000u32)"), "{src}");
    assert!(src.contains("Duration::new(0u64, 2000000u32)"), "{src}");
    assert_eq!(2, src.matches("|i: &u64| i * 2").count(), "{src}");
    assert!(!src.contains("for "), "no loop survives into the artifact");
    assert!(src.starts_with("wingfoil::nitro! {"), "{src}");
}

/// A graph the generator cannot emit fails with **every** reason at once, not
/// the first — fixing one quotation per generator run is the workflow this is
/// meant to avoid.
#[test]
fn generate_reports_all_reasons_it_cannot_emit() {
    let factor = 3u64;
    let err = codegen::generate("bad", "u64", |g| {
        let ticks = g.ticker(PERIOD).count();
        // Two erased closures, both capturing.
        let a = ticks.map(move |i: &u64| i * factor);
        a.map(move |i: &u64| i + factor)
    })
    .expect_err("erased closures must be refused");

    let text = err.to_string();
    assert!(text.contains("2 node(s)"), "both reported at once: {text}");
    assert!(text.contains("_q(func!"), "message says what to do: {text}");
}

/// The artifact header is not decoration: a **stale** generated file is the
/// failure this design cannot detect, so saying so at the top of the file is
/// the whole mitigation.
#[test]
fn write_artifact_stamps_a_generated_header() {
    let (g, _out) = wire_source_graph();
    let src = emit(&g.describe(), "target", "u64");

    let path = temp_artifact_path("header");
    codegen::write_artifact(path.to_str().expect("utf-8 temp path"), &src)
        .expect("writing the artifact");

    let written = std::fs::read_to_string(&path).expect("reading it back");
    let _ = std::fs::remove_file(&path);

    assert!(
        written.starts_with("// @generated by wingfoil::codegen"),
        "{written}"
    );
    assert!(written.contains("do not edit"), "{written}");
    assert!(written.contains("frozen at generation time"), "{written}");
    assert!(
        written.contains(&src),
        "the source survives the header: {written}"
    );
}

/// Breadcrumbs map a pass-2 compile error in generated code back to the wiring
/// that produced it — §5's mitigation for the design's roughest edge. Off by
/// default because they embed line numbers, which would make a byte-comparison
/// against a checked-in artifact fire on any edit to the wiring file.
#[test]
fn breadcrumbs_point_back_at_the_wiring() {
    let (g, _out) = wire_source_graph();
    let nodes = g.describe();

    let bare = codegen::emit(&nodes, "target", "u64", Breadcrumbs::Off).expect("emits");
    assert!(!bare.contains("// from "), "{bare}");

    let annotated = codegen::emit(&nodes, "target", "u64", Breadcrumbs::On).expect("emits");
    assert!(
        annotated.contains("// from ") && annotated.contains("codegen_emission.rs:"),
        "quoted nodes carry their origin:\n{annotated}"
    );
    // Only the quoted node has an origin to report; the ticker and count do not.
    assert_eq!(1, annotated.matches("// from ").count(), "{annotated}");
}

/// **The tail is what the wiring returned, not the last node it wired.** A side
/// sink is wired *after* the output, so defaulting to the last node would emit
/// an artifact returning the sink — and `for_each` yields `Stream<()>`, so that
/// artifact would fail pass 2 with a type error in generated code rather than
/// anywhere a reader would look.
#[test]
fn the_emitted_tail_is_the_returned_output_not_the_last_node() {
    let src = codegen::generate("with_sink", "u64", |g| {
        let double = func!(|i: &u64| i * 2);
        let out = g.ticker(PERIOD).count().map(double.f).with_src(&double);
        // Wired last, but not the output.
        let noop = func!(|_v: &u64| ::wingfoil::anyhow::Ok(()));
        let _sink = out.for_each(noop.f).with_src(&noop);
        out
    })
    .expect("emittable");

    // Four nodes: ticker, count, map, for_each. The output is the map (n2).
    assert!(
        src.contains("let n3 = n2.for_each("),
        "the sink is wired: {src}"
    );
    assert!(
        src.trim_end().ends_with("n2\n    }\n}"),
        "tail must be the returned map, not the sink:\n{src}"
    );
}

// ---------------------------------------------------------------------------
// The motivating workload: per-instrument pipelines with per-instrument
// *parameters*, which is what tier-2 captures exist for.
// ---------------------------------------------------------------------------

// The artifact the generator must produce for the two-instrument desk below.
// A real `nitro!` block, so the file compiling proves the emitted text — the
// re-materialised capture blocks included — is valid wiring source.
wingfoil::nitro! {
    fn desk_target(g: &GraphBuilder) -> Stream<f64> {
        let n0 = g.ticker(::core::time::Duration::new(0u64, 1000000u32));
        let n1 = n0.count();
        let n2 = n1.map(|n: &u64| *n as f64 * 100.0);
        let n3 = n2.map({ let fee = 2.5f64; move |p: &f64| p - fee });
        let n4 = g.ticker(::core::time::Duration::new(0u64, 5000000u32));
        let n5 = n4.count();
        let n6 = n5.map(|n: &u64| *n as f64 * 100.0);
        let n7 = n6.map({ let fee = 1.0f64; move |p: &f64| p - fee });
        let n8 = n3.join(&n7, |x: &f64, y: &f64| x + y);
        n8
    }
}

/// **Per-instrument parameters, end to end.** Before tier 2 this graph could
/// not be generated at all: the fee closure captures, so its body referenced a
/// binding that exists only in the wiring, and the generator refused it. Now
/// each leg carries its own frozen fee.
///
/// This is the shape §7 justifies the whole project with — per-instrument
/// pipelines built from a config file — and it is the case the earlier
/// per-instrument demo could not express.
#[test]
fn a_per_instrument_desk_generates_with_its_own_parameters() {
    struct Instrument {
        /// The synthetic clock's period. Named `period`, not `tick`: in a
        /// trading context a "tick" is a price increment or a single market
        /// update, neither of which is a `Duration`.
        ///
        /// It exists only because `ticker` stands in for a market-data feed —
        /// a real instrument config carries a symbol and a subscription, and
        /// data arrives when it arrives. `external`/`channel` sources are still
        /// excluded from compiled graphs, so the placeholder is load-bearing
        /// for the test rather than representative of the domain.
        period: Duration,
        fee: f64,
    }
    let cfg = [
        Instrument {
            period: Duration::from_millis(1),
            fee: 2.5,
        },
        Instrument {
            period: Duration::from_millis(5),
            fee: 1.0,
        },
    ];

    let wire = |g: &GraphBuilder| {
        let to_px = func!(|n: &u64| *n as f64 * 100.0);
        let legs: Vec<Stream<f64>> = cfg
            .iter()
            .map(|inst| {
                let px = g.ticker(inst.period).count().map(to_px.f).with_src(&to_px);
                // The per-instrument parameter, declared and frozen.
                let fee = inst.fee;
                let net = func!([fee] move |p: &f64| p - fee);
                px.map(net.f).with_src(&net)
            })
            .collect();
        legs.into_iter()
            .reduce(|a, b| {
                let add = func!(|x: &f64, y: &f64| x + y);
                a.join(&b, add.f).with_src(&add)
            })
            .expect("at least one instrument")
    };

    let src = codegen::generate("desk_target", "f64", wire).expect("desk must be emittable");
    let expected = "\
wingfoil::nitro! {
    fn desk_target(g: &GraphBuilder) -> Stream<f64> {
        let n0 = g.ticker(::core::time::Duration::new(0u64, 1000000u32));
        let n1 = n0.count();
        let n2 = n1.map(|n: &u64| *n as f64 * 100.0);
        let n3 = n2.map({ let fee = 2.5f64; move |p: &f64| p - fee });
        let n4 = g.ticker(::core::time::Duration::new(0u64, 5000000u32));
        let n5 = n4.count();
        let n6 = n5.map(|n: &u64| *n as f64 * 100.0);
        let n7 = n6.map({ let fee = 1.0f64; move |p: &f64| p - fee });
        let n8 = n3.join(&n7, |x: &f64, y: &f64| x + y);
        n8
    }
}";
    assert_eq!(expected, src);

    // Parity: the artifact computes what the graph it came from computes.
    let g = GraphBuilder::new();
    let acc = wire(&g).with_time().accumulate();
    let mut runner = g.build();
    runner.run(HISTORICAL, RunFor::Cycles(6)).unwrap();
    let source_graph = runner.value(acc);

    let g2 = GraphBuilder::new();
    let acc2 = desk_target::nested(&g2).with_time().accumulate();
    let mut runner2 = g2.build();
    runner2.run(HISTORICAL, RunFor::Cycles(6)).unwrap();

    assert_eq!(source_graph, runner2.value(acc2));
    assert!(!source_graph.is_empty(), "the desk produced values");
}

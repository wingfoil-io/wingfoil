//! **SPIKE (#726 step 3)** — can a walker print a valid `nitro!` wiring fn from
//! a wired graph?
//!
//! The last discovery-shaped question in the two-pass design. Step 2 (`func!` +
//! node metadata, landed) proved a graph can say *what its nodes compute*. This
//! asks the next thing: is `describe()` enough to reconstruct *wiring source* —
//! and does that source compile and compute the same values?
//!
//! # How this proves pass 2 without running a second compiler
//!
//! A generator's output is a file a later `cargo build` compiles, and a test
//! cannot easily invoke rustc on generated text. So the intended artifact sits
//! in *this file* as a real `nitro!` block ([`target`]), and the walker must
//! emit **exactly that text**. If the strings match and the block compiles, the
//! emitted wiring is valid by construction — and
//! [`emitted_graph_matches_the_interpreted_one`] then checks the artifact
//! agrees with the graph it came from, in values *and* tick times, which is the
//! parity obligation §8 step 3 asks for.
//!
//! # The answer, up front
//!
//! **Topology emission works.** Wiring order, edges, source-vs-combinator, the
//! method name, and quoted closure bodies are all recoverable, and the artifact
//! is byte-identical to hand-written `nitro!` input.
//!
//! **Config emission does not exist.** `data_cfg` below is a hole the *caller*
//! has to fill: nothing in the node metadata carries a `ticker`'s `Duration`, a
//! `fold`'s seed, or a `limit`'s bound. That hole is precisely `EmitLiteral`
//! (§3), and it is the bulk of what step 3 still has to build. The tests at the
//! bottom pin each gap so none of them can regress into a silent partial
//! emission.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Duration;

use wingfoil::func;
use wingfoil::interp::NodeInfo;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const PERIOD: Duration = Duration::from_millis(1);
const RUN: RunFor = RunFor::Cycles(5);

// ---------------------------------------------------------------------------
// The walker.
// ---------------------------------------------------------------------------

/// Why a node could not be emitted, with enough detail to point at the wiring
/// that produced it — the "loud, precise failure" of §4.1.
#[derive(Debug, PartialEq, Eq)]
struct Ineligible {
    index: usize,
    label: &'static str,
    reason: String,
}

/// Walk a described graph and print a `nitro!` wiring function.
///
/// `data_cfg` supplies the source text of each node's **non-closure** config,
/// by node index. It is a parameter rather than something read off the graph
/// because the graph does not have it — see the module docs. Closure configs
/// come from the node's own `src`, recorded by `func!` + `with_src`.
///
/// Returns the source text, or every reason it could not be produced. A
/// *partial* artifact would be worse than a failure: it would compile into a
/// graph quietly missing nodes.
fn emit(
    nodes: &[NodeInfo],
    fn_name: &str,
    out_ty: &str,
    data_cfg: &HashMap<usize, &str>,
) -> Result<String, Vec<Ineligible>> {
    let mut bad = Vec::new();
    for n in nodes {
        let Some(build) = n.build else {
            bad.push(Ineligible {
                index: n.index,
                label: n.label,
                reason: "hand-written node: no `#[op(build = ..)]` method name to emit".into(),
            });
            continue;
        };
        if !n.passive_ups.is_empty() {
            bad.push(Ineligible {
                index: n.index,
                label: n.label,
                reason: format!("`{build}` has passive edges; emission order is unproven"),
            });
        }
        if n.active_ups.len() > 1 {
            bad.push(Ineligible {
                index: n.index,
                label: n.label,
                reason: format!("`{build}` is multi-edge; receiver/argument split unproven"),
            });
        }
    }
    if !bad.is_empty() {
        return Err(bad);
    }

    let mut s = String::new();
    let _ = writeln!(s, "wingfoil::nitro! {{");
    let _ = writeln!(
        s,
        "    fn {fn_name}(g: &GraphBuilder) -> Stream<{out_ty}> {{"
    );
    for n in nodes {
        let build = n.build.expect("checked above");
        // A closure config is emitted as a *literal* at the generated call
        // site — exactly the inference root `nitro!` relies on, and the reason
        // quotation had to keep the tokens (§2). A data config comes from the
        // caller-supplied table; `""` is a genuinely config-free op.
        let arg = n
            .src
            .or_else(|| data_cfg.get(&n.index).copied())
            .unwrap_or("");
        match n.active_ups.first() {
            None => {
                let _ = writeln!(s, "        let n{} = g.{build}({arg});", n.index);
            }
            Some(&up) => {
                let _ = writeln!(s, "        let n{} = n{up}.{build}({arg});", n.index);
            }
        }
    }
    let last = nodes.last().expect("non-empty graph");
    let _ = writeln!(s, "        n{}", last.index);
    let _ = writeln!(s, "    }}");
    let _ = write!(s, "}}");
    Ok(s)
}

// ---------------------------------------------------------------------------
// The artifact the walker must produce — a real `nitro!` block, so "the emitted
// text is valid Rust" is proven by this file compiling.
// ---------------------------------------------------------------------------

wingfoil::nitro! {
    fn target(g: &GraphBuilder) -> Stream<u64> {
        let n0 = g.ticker(PERIOD);
        let n1 = n0.count();
        let n2 = n1.map(|i: &u64| i * 2);
        n2
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

/// The `EmitLiteral` hole, filled by hand: the ticker's period. Everything else
/// in this graph is either config-free or a quoted closure.
fn hand_written_configs() -> HashMap<usize, &'static str> {
    HashMap::from([(0usize, "PERIOD")])
}

// ---------------------------------------------------------------------------
// What works.
// ---------------------------------------------------------------------------

#[test]
fn the_walker_emits_the_expected_wiring_source() {
    let (g, _out) = wire_source_graph();
    let emitted =
        emit(&g.describe(), "target", "u64", &hand_written_configs()).expect("should be emittable");

    // Byte-identical to the `nitro!` block above, which compiles — so the
    // emitted text is valid wiring source, not merely plausible-looking.
    let expected = "\
wingfoil::nitro! {
    fn target(g: &GraphBuilder) -> Stream<u64> {
        let n0 = g.ticker(PERIOD);
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

/// **Gap 1: data configs are not recorded.** Node metadata carries the closure
/// source (step 2) but nothing about a non-closure config — the `Duration`
/// here, a `fold` seed, a `limit` bound. That is `EmitLiteral` (§3), and it is
/// the bulk of what step 3 still has to build; without it a ticker emits as
/// `ticker()`, which does not compile.
#[test]
fn data_configs_are_not_recoverable_from_the_graph() {
    let (g, _out) = wire_source_graph();
    let nodes = g.describe();
    assert_eq!(Some("ticker"), nodes[0].build);
    assert_eq!(
        None, nodes[0].src,
        "a ticker's Duration is nowhere in the node metadata"
    );

    // Without the caller's table, the ticker loses its period.
    let emitted = emit(&nodes, "target", "u64", &HashMap::new()).expect("no structural blockers");
    assert!(
        emitted.contains("g.ticker();"),
        "expected a config-less ticker, got: {emitted}"
    );
}

/// **Gap 2: an unquoted closure is indistinguishable from no closure.** Both
/// report `src: None`, so the walker cannot tell `count()` (genuinely
/// config-free) from `map(move |i| i * factor)` (a capturing closure the engine
/// erased). It therefore emits `map()` — which does not compile — instead of
/// refusing.
///
/// §4.1 wants a loud error naming every non-emittable node *and its call site*.
/// That needs metadata this spike does not have: a per-node "takes a closure
/// config" flag (`#[op]` knows it) plus the `#[track_caller]` location the
/// design specifies. Cheap to add; currently absent.
#[test]
fn an_unquoted_closure_is_indistinguishable_from_no_config() {
    let g = GraphBuilder::new();
    let factor = 3u64;
    let _out = g
        .ticker(PERIOD)
        .count()
        // Captures `factor` — erased, unrecoverable.
        .map(move |i: &u64| i * factor);

    let nodes = g.describe();
    assert_eq!(None, nodes[1].src, "count genuinely has no config");
    assert_eq!(None, nodes[2].src, "an erased closure looks identical");

    let emitted = emit(&nodes, "bad", "u64", &hand_written_configs()).expect("no structural block");
    assert!(
        emitted.contains("n1.map();"),
        "silently emits an uncompilable call rather than refusing: {emitted}"
    );
}

/// **Gap 3: multi-edge and passive-edge ops are refused.** `active_ups`
/// preserves receiver-first order, so `join` is *probably* recoverable as
/// `a.join(&b, f)` — but nothing distinguishes a receiver from an argument, and
/// `sample`'s passive leg is absent from `active_ups` entirely. Both need
/// proving before the walker can claim general coverage; until then it fails
/// loudly, which is the right default.
#[test]
fn multi_edge_ops_are_refused_with_reasons() {
    let g = GraphBuilder::new();
    let a = g.ticker(PERIOD).count();
    let b = a.map(|i: &u64| i * 10);
    let combine = func!(|x: &u64, y: &u64| x + y);
    let _joined = a.join(&b, combine.f).with_src(&combine);

    let err = emit(&g.describe(), "joined", "u64", &hand_written_configs())
        .expect_err("join must be refused");
    assert_eq!(1, err.len());
    assert_eq!("Join", err[0].label);
    assert!(err[0].reason.contains("multi-edge"), "{:?}", err[0]);
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

    let emitted = emit(&g.describe(), "unrolled", "u64", &hand_written_configs()).expect("emits");
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

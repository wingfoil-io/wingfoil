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
//! **Config emission works too, now that `EmitLiteral` exists.** A node's data
//! config — a `ticker`'s `Duration`, a `limit`'s bound — is rendered to source
//! by `Stream::with_cfg` and read back off the node, so the walker needs no
//! caller-supplied table.
//!
//! **Eligibility is decidable.** `#[op]` records whether an op's `Cfg` is a
//! closure, so `takes_closure_cfg && src.is_none()` states exactly "this node
//! has a closure the engine erased and the wiring did not quote" — the case an
//! emitter must refuse. Neither field alone says it, because a config-free op
//! like `count` also reports `src: None`.
//!
//! What is left: emission covers **single-edge chains only**. Multi-edge
//! (`join`) and passive-edge (`sample`) ops are refused rather than emitted,
//! and the refusals name node indices rather than the *call sites*
//! `#[track_caller]` would give. Both are pinned by tests at the bottom so
//! neither can regress into a silent partial emission.

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
/// Both kinds of config come off the node: closure bodies from `src` (recorded
/// by `func!` + `with_src`) and data values from `cfg_src` (rendered by
/// `EmitLiteral` via `with_cfg`).
///
/// Returns the source text, or every reason it could not be produced. A
/// *partial* artifact would be worse than a failure: it would compile into a
/// graph quietly missing nodes.
fn emit(nodes: &[NodeInfo], fn_name: &str, out_ty: &str) -> Result<String, Vec<Ineligible>> {
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
        // The precise statement of "erased closure": the op takes one, and the
        // wiring did not quote it. Neither field alone says this — a
        // config-free op like `count` also reports `src: None`.
        if n.takes_closure_cfg && n.src.is_none() {
            bad.push(Ineligible {
                index: n.index,
                label: n.label,
                reason: format!(
                    "`{build}`'s closure was not quoted; wrap it in `func!` and \
                     record it with `.with_src(..)` to make this node emittable"
                ),
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
        let arg: String = match (n.cfg_src.as_deref(), n.src) {
            // An op with both, e.g. `fold(seed, f)` — data first, matching
            // every such signature in the catalog.
            (Some(cfg), Some(src)) => format!("{cfg}, {src}"),
            (Some(cfg), None) => cfg.to_string(),
            (None, Some(src)) => src.to_string(),
            (None, None) => String::new(),
        };
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
        let n0 = g.ticker(::core::time::Duration::new(0u64, 1000000u32));
        let n1 = n0.count();
        let n2 = n1.map(|i: &u64| i * 2);
        n2
    }
}

/// The same graph, wired procedurally — what a generator's pass 1 consumes.
fn wire_source_graph() -> (GraphBuilder, Stream<u64>) {
    let g = GraphBuilder::new();
    let ticks = g.ticker(PERIOD).with_cfg(&PERIOD).count();
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
    let emitted = emit(&g.describe(), "target", "u64").expect("should be emittable");

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

    let emitted = emit(&nodes, "target", "u64").expect("emits");
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
        .with_cfg(&PERIOD)
        .count()
        // Captures `factor` — erased, unrecoverable.
        .map(move |i: &u64| i * factor);

    let nodes = g.describe();
    assert!(!nodes[1].takes_closure_cfg, "count has no closure config");
    assert!(nodes[2].takes_closure_cfg, "map does");
    assert_eq!(None, nodes[2].src, "and it was not quoted");

    let err = emit(&nodes, "bad", "u64").expect_err("must refuse the erased closure");
    assert_eq!(1, err.len(), "only the map is ineligible: {err:?}");
    assert_eq!(2, err[0].index);
    assert!(err[0].reason.contains("func!"), "{:?}", err[0]);
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

    emit(&nodes, "target", "u64").expect("a fully quoted graph still emits");
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
    let a = g.ticker(PERIOD).with_cfg(&PERIOD).count();
    let scale = func!(|i: &u64| i * 10);
    let b = a.map(scale.f).with_src(&scale);
    let combine = func!(|x: &u64, y: &u64| x + y);
    let _joined = a.join(&b, combine.f).with_src(&combine);

    // Everything else in this graph is quoted or config-free, so the join is
    // the *only* refusal — which is what makes this a test of the multi-edge
    // rule rather than of eligibility in general.
    let err = emit(&g.describe(), "joined", "u64").expect_err("join must be refused");
    assert_eq!(1, err.len(), "only the join is ineligible: {err:?}");
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

    let emitted = emit(&g.describe(), "unrolled", "u64").expect("emits");
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

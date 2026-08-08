//! Generate compiled-graph source by **running** the wiring.
//!
//! Pass 1 executes an ordinary Rust wiring function against the interpreted
//! builder; this module walks the graph it produced and prints a `nitro!`
//! wiring function. Pass 2 is an ordinary `cargo build` over that artifact,
//! which turns it into the monomorphized runner.
//!
//! The point is the one thing `nitro!` structurally cannot do: **topology
//! decided by running code**. A macro sees tokens, not a config file — so a
//! loop over N discovered instruments is inexpressible. Pass 1 *runs* the loop,
//! so the generator emits the N unrolled pipelines it produced, and `nitro!`
//! never sees a loop at all.
//!
//! Design: `docs/wired-graph-codegen-decision.md`; sequencing: #726.
//!
//! # What the artifact is
//!
//! Plain, reviewable Rust: a `nitro!` invocation with the topology unrolled and
//! every config inlined. `nitro!` stays the single backend, so there is exactly
//! one place in the system that knows how to turn wiring into a monomorphized
//! runner — the generator does not emit forwarder calls, state locals or kernel
//! loops itself, and cannot drift from the macro that does.
//!
//! ```no_run
//! use std::time::Duration;
//! use wingfoil::codegen;
//! use wingfoil::func;
//! use wingfoil::prelude::*;
//!
//! # fn main() -> wingfoil::anyhow::Result<()> {
//! let periods = [Duration::from_millis(1), Duration::from_millis(5)];
//!
//! let src = codegen::generate("desk", "u64", |g| {
//!     let mut last = None;
//!     for p in periods {
//!         last = Some(g.ticker(p).count().map_q(func!(|i: &u64| i * 2)));
//!     }
//!     last.expect("at least one period")
//! })?;
//!
//! codegen::write_artifact("src/desk.gen.rs", &src)?;
//! # Ok(())
//! # }
//! ```
//!
//! # What it requires of the wiring
//!
//! **Closures must be recorded.** The engine erases them, so an unrecorded
//! closure is simply gone by the time a traversal sees it — nothing can recover
//! it. The cheapest way to record every one is to put
//! [`#[wiring]`](crate::wiring) on the wiring function and write ordinary Rust:
//! it rewrites each closure-carrying call to keep the tokens, and detects
//! captures too, so `move |p| p - fee` needs no declaration. Otherwise record
//! them one at a time — the op's `_q` twin with a [`func!`](crate::func)
//! quotation (`.map_q(func!(|x: &f64| x * 2.0))`), or `func!` plus
//! [`Stream::with_src`](crate::fluent::Stream::with_src) for an op without a
//! twin.
//!
//! **Data configs mostly look after themselves.** A config is never erased —
//! the value is right there in the op's cell — so an op with a *concrete*
//! config carries `#[op(emit_cfg)]` and records it during ordinary wiring:
//! `g.ticker(period)` needs no annotation. Only ops whose config type is
//! *generic* (`fold`'s and `scan`'s seeds) need
//! [`Stream::with_cfg`](crate::fluent::Stream::with_cfg), because an
//! [`EmitLiteral`](crate::emit::EmitLiteral) bound on their public signature
//! would forbid folding into an accumulator the generator cannot render.
//!
//! Anything missing is a **refusal**, never a partial artifact — see
//! [`Ineligible`]. A partial emission would compile into a graph quietly
//! missing nodes, which is a far worse failure than not producing a file.
//!
//! # Standing hazards
//!
//! - **Pass-1 wiring must be deterministic** and re-runnable in the generation
//!   environment. Defer-to-start makes wiring side-effect-free, which is most
//!   of this.
//! - **Emitted configs are frozen.** The artifact carries pass 1's
//!   configuration; changing it means regenerate and recompile.
//! - **Stale generation** is the failure with no runtime symptom: an artifact
//!   built from last quarter's parameters compiles, runs, and produces
//!   plausible numbers. Two things guard it. The artifact is `nitro!` *input* —
//!   diffable and reviewable rather than expanded runner code — and
//!   [`check_artifact`] turns "is it current?" into a one-line test that fails
//!   the build. Use the second; the first only helps someone who is already
//!   looking.

use std::fmt;
use std::fmt::Write as _;

use anyhow::{Context, Result};

use crate::fluent::{GraphBuilder, Stream};
use crate::interp::NodeInfo;

/// Why a node could not be emitted.
///
/// Carries the node's index and op so the report names something a reader can
/// find. What it does *not* yet carry is the **call site** — the line of wiring
/// that produced the node — which would need `#[track_caller]` on the generated
/// wiring methods. Until then the `loc` of a quoted sibling is often the
/// closest landmark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ineligible {
    /// Wiring order index, matching [`NodeInfo::index`].
    pub index: usize,
    /// The op's type name, as it appears in engine error messages.
    pub label: &'static str,
    /// What is missing, phrased as something to do about it.
    pub reason: String,
}

impl fmt::Display for Ineligible {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "node {} ({}): {}", self.index, self.label, self.reason)
    }
}

/// Every reason a graph could not be emitted, reported together.
///
/// All of them, not the first: a user fixing one quotation at a time through
/// repeated generator runs is the workflow this exists to avoid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotEmittable(pub Vec<Ineligible>);

impl fmt::Display for NotEmittable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{} node(s) of this graph cannot be emitted:",
            self.0.len()
        )?;
        for bad in &self.0 {
            writeln!(f, "  - {bad}")?;
        }
        write!(
            f,
            "Every closure a generated graph contains has to be recorded, because \
             the engine erases closures and no traversal can recover one. \
             `#[wiring]` on the wiring function records all of them, captures \
             included; `func!` records one at a time. Data configs mostly look \
             after themselves — only `fold`/`scan` seeds, whose type is generic, \
             still need `.with_cfg(&seed)`."
        )
    }
}

impl std::error::Error for NotEmittable {}

/// Whether to annotate each emitted node with the wiring location its
/// quotation came from.
///
/// Off by default, and worth knowing why: the breadcrumbs are genuinely useful
/// when a pass-2 compile error lands in generated code (§5's mitigation for the
/// design's roughest edge), but they embed **line numbers**, so any edit to the
/// wiring file changes the artifact even when the graph did not. That makes a
/// byte-comparison against a checked-in artifact fire spuriously.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Breadcrumbs {
    /// Emit bare wiring statements.
    #[default]
    Off,
    /// Append `// from <file>:<line>` to each node whose closure was quoted.
    On,
}

/// Run `wire` against a fresh graph and emit `nitro!` source for what it built.
///
/// `fn_name` names the generated wiring function; `out_ty` is its output type,
/// written verbatim into the signature.
///
/// **`out_ty` is a parameter rather than derived from `T`** because
/// `std::any::type_name` is explicitly not guaranteed to produce valid source:
/// it renders `Vec<u64>` as `alloc::vec::Vec<u64>`, which does not compile.
/// Deriving it would work for the primitives and fail on everything else, which
/// is a worse failure than asking.
pub fn generate<T, F>(fn_name: &str, out_ty: &str, wire: F) -> Result<String>
where
    F: FnOnce(&GraphBuilder) -> Stream<T>,
{
    generate_with(fn_name, out_ty, Breadcrumbs::default(), wire)
}

/// [`generate`] with control over [`Breadcrumbs`].
pub fn generate_with<T, F>(
    fn_name: &str,
    out_ty: &str,
    breadcrumbs: Breadcrumbs,
    wire: F,
) -> Result<String>
where
    F: FnOnce(&GraphBuilder) -> Stream<T>,
{
    // Pass 1: run the wiring. Side-effect-free by the defer-to-start rule, so
    // running it here does no I/O and opens no sockets.
    let g = GraphBuilder::new();
    let out = wire(&g);
    let nodes = g.describe();
    // The tail is what the wiring *returned*, not the last node it happened to
    // wire — a graph with a side sink ends on the sink.
    emit_with_tail(&nodes, fn_name, out_ty, out.handle().index(), breadcrumbs)
}

/// [`emit_with_tail`] defaulting the tail to the last node wired. Correct only
/// when the graph's output *is* the last node — see `emit_with_tail`.
pub fn emit(
    nodes: &[NodeInfo],
    fn_name: &str,
    out_ty: &str,
    breadcrumbs: Breadcrumbs,
) -> Result<String> {
    let tail = nodes.len().saturating_sub(1);
    emit_with_tail(nodes, fn_name, out_ty, tail, breadcrumbs)
}

/// Print a `nitro!` wiring function for an already-described graph, with `tail`
/// as its output node.
///
/// Separate from [`generate`] so a caller that already holds a description —
/// a debugger, a test — can emit without re-running the wiring.
///
/// **`tail` is explicit, not "the last node wired"**, because those differ the
/// moment a graph has a side sink: `let out = ..; out.for_each(..);` leaves the
/// sink last in wiring order while the output is the node before it. Defaulting
/// to the last node would emit an artifact returning the wrong stream — and
/// one that still compiles, if the types happen to line up.
pub fn emit_with_tail(
    nodes: &[NodeInfo],
    fn_name: &str,
    out_ty: &str,
    tail: usize,
    breadcrumbs: Breadcrumbs,
) -> Result<String> {
    let bad = ineligible(nodes);
    if !bad.is_empty() {
        return Err(NotEmittable(bad).into());
    }
    if nodes.is_empty() {
        anyhow::bail!("cannot emit an empty graph: `{fn_name}` wired no nodes");
    }
    anyhow::ensure!(
        tail < nodes.len(),
        "tail node {tail} is out of range for a graph of {} nodes",
        nodes.len()
    );

    let mut s = String::new();
    let _ = writeln!(s, "wingfoil::nitro! {{");
    let _ = writeln!(
        s,
        "    fn {fn_name}(g: &GraphBuilder) -> Stream<{out_ty}> {{"
    );
    for n in nodes {
        let build = n.build.expect("invariant: eligibility checked above");
        let arg = config_args(n);
        // Edges in the order the original call listed them — receiver first,
        // then each `&stream` argument, then the config. Reconstructed via the
        // passive mask; the partitioned active/passive lists alone cannot say
        // which position each edge occupied.
        let edges = n.edges_in_call_order();
        let args = match edges.split_first() {
            None => arg,
            Some((_, rest)) => {
                let mut parts: Vec<String> = rest.iter().map(|u| format!("&n{u}")).collect();
                if !arg.is_empty() {
                    parts.push(arg);
                }
                parts.join(", ")
            }
        };
        let trail = match (breadcrumbs, n.loc) {
            (Breadcrumbs::On, Some((file, line))) => format!(" // from {file}:{line}"),
            _ => String::new(),
        };
        match edges.first() {
            None => {
                let _ = writeln!(s, "        let n{} = g.{build}({args});{trail}", n.index);
            }
            Some(recv) => {
                let _ = writeln!(
                    s,
                    "        let n{} = n{recv}.{build}({args});{trail}",
                    n.index
                );
            }
        }
    }
    let _ = writeln!(s, "        n{tail}");
    let _ = writeln!(s, "    }}");
    let _ = write!(s, "}}");
    Ok(s)
}

/// The config arguments of one node, in call order: data first, then the
/// closure — matching every such signature in the catalog (`fold(seed, f)`).
fn config_args(n: &NodeInfo) -> String {
    match (n.cfg_src.as_deref(), n.src.as_deref()) {
        (Some(cfg), Some(src)) => format!("{cfg}, {src}"),
        (Some(cfg), None) => cfg.to_string(),
        (None, Some(src)) => src.to_string(),
        (None, None) => String::new(),
    }
}

/// Every node that cannot be emitted, with the reason.
///
/// Public so a caller can ask *before* committing to generation — a build
/// script reporting coverage, or a test asserting a graph stays emittable.
pub fn ineligible(nodes: &[NodeInfo]) -> Vec<Ineligible> {
    let mut bad = Vec::new();
    for n in nodes {
        let Some(build) = n.build else {
            bad.push(Ineligible {
                index: n.index,
                label: n.label,
                reason: "hand-written node: no `#[op(build = ..)]` method name to emit. \
                         The variadic ops (`merge_all`, `combine`) are the catalog's \
                         examples — their forwarders are hand-written."
                    .into(),
            });
            continue;
        };
        // Checked before the quotation check, and reported instead of it: a
        // node with an unrenderable capture *is* quoted, so the "quote your
        // closure" advice would send a reader to fix something already done.
        if !n.unresolved_captures.is_empty() {
            let names = n
                .unresolved_captures
                .iter()
                .map(|c| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(", ");
            bad.push(Ineligible {
                index: n.index,
                label: n.label,
                reason: format!(
                    "the closure captures {names}, whose value cannot be rendered as \
                     Rust source — no `EmitLiteral` impl. An artifact would refer to \
                     bindings that exist only in the wiring. Either compute a \
                     renderable value (a primitive, `String`, `Duration`, …) outside \
                     the closure and capture that, or keep this node out of the \
                     generated graph."
                ),
            });
            continue;
        }
        // A seeded accumulator whose seed was never recorded. Checked
        // separately from the closure, because for `fold`/`scan` the `Cfg` *is*
        // the closure — quoting it says nothing about the seed, and emitting
        // without one prints `.fold(f)`, silently dropping the accumulator's
        // starting value. That artifact fails at pass 2 on arity, but the
        // refusal belongs here: a partial emission must never be produced.
        if n.has_init_arg && n.cfg_src.is_none() {
            bad.push(Ineligible {
                index: n.index,
                label: n.label,
                reason: format!(
                    "`{build}` takes a seed at the call site (`{build}(seed, ..)`) and the \
                     wiring did not record it, so emitting would drop it. Add \
                     `.with_cfg(&seed)`. The seed is not covered by `#[op(emit_cfg)]` \
                     because its type is generic — an `EmitLiteral` bound would land on \
                     the public signature and forbid accumulating into anything this \
                     module cannot render."
                ),
            });
            continue;
        }
        // The precise statement of "erased closure": the op takes one, and the
        // wiring did not quote it. Neither fact alone says this — a config-free
        // op like `count` also reports no source.
        if n.takes_closure_cfg && n.src.is_none() {
            bad.push(Ineligible {
                index: n.index,
                label: n.label,
                reason: format!(
                    "`{build}`'s closure was not quoted, so the engine erased it. \
                     Write `.{build}_q(func!(..))` — or, if it captures, \
                     declare what it captures: \
                     `.{build}_q(func!([threshold] move |v| ..))`."
                ),
            });
        }
    }
    bad
}

/// The banner every artifact carries. Fixed text, so [`check_artifact`] can
/// strip it and compare only the wiring.
const HEADER: &str = "\
// @generated by wingfoil::codegen — do not edit.
//
// Produced by running a wiring function and walking the graph it built, so
// every config here is frozen at generation time. Regenerate rather than
// editing: a hand edit is lost on the next run.
//
// Nothing detects at *runtime* that this file is stale. Guard it with a test
// that calls `codegen::check_artifact` — see that function's docs.

";

/// Render an artifact's full file contents: banner plus wiring.
fn artifact_text(source: &str) -> String {
    format!("{HEADER}{source}\n")
}

/// Write an artifact to disk with a generated-file header.
///
/// Pair it with [`check_artifact`] in a test. The header says "do not edit",
/// which is advice; the test is enforcement.
pub fn write_artifact(path: &str, source: &str) -> Result<()> {
    std::fs::write(path, artifact_text(source))
        .with_context(|| format!("writing generated artifact to {path}"))
}

/// Fail if the artifact at `path` is not exactly what `source` would write.
///
/// **This is the mitigation for the one failure the design cannot otherwise
/// detect.** Everything here freezes configuration into code, so an artifact
/// generated from last quarter's fees still compiles, still runs, and still
/// produces plausible numbers. Nothing about the running system reveals it.
///
/// Put generation in a test and check the checked-in file against it:
///
/// ```no_run
/// # use wingfoil::codegen;
/// # use wingfoil::prelude::*;
/// # fn desk(g: &GraphBuilder) -> Stream<u64> { g.ticker(std::time::Duration::from_millis(1)).count() }
/// #[test]
/// fn the_generated_desk_is_current() -> wingfoil::anyhow::Result<()> {
///     let src = codegen::generate("desk", "u64", desk)?;
///     codegen::check_artifact("src/desk.gen.rs", &src)
/// }
/// ```
///
/// Because pass 1 is deterministic and re-runnable, that single assertion
/// catches **both** ways an artifact goes wrong: a config change nobody
/// regenerated for, and a hand edit somebody made to the generated file. Both
/// surface as a failing test naming the file, rather than as a wrong number in
/// production.
///
/// It cannot catch a config change that the *test's* own inputs do not see — if
/// the wiring reads a file at generation time, the test must read the same
/// file. That is the residue, and it is a property of where the config lives
/// rather than of this check.
pub fn check_artifact(path: &str, source: &str) -> Result<()> {
    let found = std::fs::read_to_string(path)
        .with_context(|| format!("reading generated artifact {path} to check it is current"))?;
    let expected = artifact_text(source);
    if found == expected {
        return Ok(());
    }
    let detail = match found.strip_prefix(HEADER) {
        // Header intact, wiring differs: either the config moved or someone
        // edited the graph by hand.
        Some(body) => {
            let (found_line, want_line) = first_difference(body.trim_end(), source.trim_end());
            format!(
                "the wiring differs from what the generator now produces.\n  \
                 generated: {want_line}\n  \
                 on disk:   {found_line}"
            )
        }
        None => "the file is missing its @generated header — it may not be an \
                 artifact, or the banner was edited."
            .to_string(),
    };
    anyhow::bail!(
        "generated artifact {path} is out of date: {detail}\n\
         Regenerate it (`codegen::write_artifact`) and commit the result. This \
         check exists because a stale artifact carries the configuration it was \
         generated with — it compiles and runs, and nothing else reveals it."
    )
}

/// The first line at which two artifacts diverge, for a message that points at
/// something rather than dumping two files.
fn first_difference(found: &str, want: &str) -> (String, String) {
    let mut f = found.lines();
    let mut w = want.lines();
    loop {
        match (f.next(), w.next()) {
            (Some(a), Some(b)) if a == b => continue,
            (a, b) => {
                return (
                    a.unwrap_or("<end of file>").trim().to_string(),
                    b.unwrap_or("<end of file>").trim().to_string(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_emittable_reports_every_reason_at_once() {
        let err = NotEmittable(vec![
            Ineligible {
                index: 2,
                label: "Map",
                reason: "closure not quoted".into(),
            },
            Ineligible {
                index: 5,
                label: "MergeN",
                reason: "hand-written".into(),
            },
        ]);
        let text = err.to_string();
        assert!(text.contains("2 node(s)"), "{text}");
        assert!(text.contains("node 2 (Map)"), "{text}");
        assert!(text.contains("node 5 (MergeN)"), "{text}");
        assert!(
            text.contains("quoted"),
            "the footer says what to do about it: {text}"
        );
        // Concrete data configs record themselves now (`#[op(emit_cfg)]`), so
        // blanket `.with_cfg(..)` advice would send a reader to fix something
        // that is not broken. Only the generic-seed case is still named.
        assert!(
            text.contains("fold"),
            "the remaining `with_cfg` case is scoped, not blanket: {text}"
        );
    }
}

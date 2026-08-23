//! Static graph introspection: the wired topology as data, and as pictures.
//!
//! A [`GraphSnapshot`] is the shape of a graph — every node, its op label, how
//! it is activated, and every edge — captured without running anything. Take
//! one from a [`GraphBuilder`](crate::fluent::GraphBuilder) before
//! [`build`](crate::fluent::GraphBuilder::build), or from a
//! [`Runner`](crate::interp::Runner) after it:
//!
//! ```
//! use std::time::Duration;
//! use wingfoil::prelude::*;
//!
//! let g = GraphBuilder::new();
//! let count = g.ticker(Duration::from_millis(10)).count();
//! let _doubled = count.map(|n: &u64| n * 2);
//!
//! let snap = g.snapshot();
//! assert_eq!(snap.node_count(), 3);
//! println!("{}", snap.to_mermaid());
//! ```
//!
//! # Why this is not legacy's `Graph::export`
//!
//! Legacy wingfoil wrote a GML file and nothing else (deviation register
//! **C6**, where the drop is recorded along with the intent to replace it with
//! a designed introspection surface rather than port it as-is). This module is
//! that replacement, and it is a superset:
//!
//! - the topology is a **value** you can inspect, assert on and serialize —
//!   not only a side effect on a file path;
//! - **active and passive edges are distinguished** ([`EdgeKind`]), which is
//!   the fact a wingfoil user most often wants a picture *for*. Legacy's GML
//!   flattened both into one undifferentiated edge;
//! - each node carries its [`Activation`], so sources, timers and busy-poll
//!   nodes are identifiable rather than being unlabelled boxes;
//! - five output formats, including [`to_gml`](GraphSnapshot::to_gml) for
//!   legacy parity and [`to_mermaid`](GraphSnapshot::to_mermaid), which GitHub
//!   renders inline in a README or a pull request.
//!
//! # Cost
//!
//! Taking a snapshot allocates and walks the node list once. It reads no
//! clock, touches no value slot and runs no op, so it cannot perturb a
//! measurement — but it is still a wiring-time/debug facility, not something
//! to call from inside a cycle.
//!
//! # Compiled and nested tiers are opaque
//!
//! A snapshot is taken from the interpreted node table, and the other two
//! `nitro!` tiers do not have one — their state lives in local variables, and
//! that absence *is* the speed. So:
//!
//! - a **`nested()` island** is a single node of the outer graph and snapshots
//!   as one, labelled `graph`. Its interior is not visible, and neither are
//!   the edges inside it;
//! - a **`compiled()`** graph has no [`Builder`](crate::interp::Builder) at
//!   all, so there is nothing to snapshot.
//!
//! This is inherent rather than deferred work — you cannot have both "the
//! optimiser erased the node boundaries" and "show me the node boundaries".
//! Run interpreted for the full picture.
//!
//! # What this deliberately does not do
//!
//! A snapshot is *structure only*. It carries no values, no tick counts and no
//! timings: those need a running graph and cost something to collect, so they
//! belong to a separate opt-in surface. See `docs/planning/introspection-plan.md` for
//! how per-node profiling and live browser devtools build on this data model.

use std::fmt::Write as _;

use crate::op::Activation;

/// Which kind of edge joins two nodes.
///
/// The distinction is the whole point of drawing a wingfoil graph: an active
/// edge propagates ticks, a passive one does not. A node reached only by
/// passive edges never fires on its own — it is read, by a downstream node
/// that some *other* edge activated. Getting this backwards is the most
/// common wiring mistake, and it is invisible in the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    /// The upstream's tick activates the downstream — the ordinary data edge.
    Active,
    /// The downstream *reads* the upstream's value but is not triggered by it
    /// (`sample`'s data leg, `bimap`/`trimap`'s inactive input). Still forces
    /// dispatch ordering: the reader runs after the value it reads.
    Passive,
}

impl EdgeKind {
    /// `"active"` / `"passive"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Active => "active",
            EdgeKind::Passive => "passive",
        }
    }
}

/// One edge of a [`GraphSnapshot`], as a flat `(from, to, kind)` triple.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Edge {
    /// Index of the upstream node.
    pub from: usize,
    /// Index of the downstream node.
    pub to: usize,
    /// Whether the upstream's tick activates `to`.
    pub kind: EdgeKind,
}

/// One node of a [`GraphSnapshot`], and the value
/// [`Runner::describe`](crate::interp::Runner::describe) yields per node.
///
/// It carries two halves, and they answer different questions. The
/// **topology** — index, label, activation, edges, `removed` — is what the
/// renderers draw and what a reader wants from a picture. The **wiring
/// record** — [`src`](Self::src), [`loc`](Self::loc), [`build`](Self::build),
/// [`cfg_src`](Self::cfg_src), [`takes_closure_cfg`](Self::takes_closure_cfg),
/// [`passive_mask`](Self::passive_mask), [`has_init_arg`](Self::has_init_arg),
/// [`unresolved_captures`](Self::unresolved_captures) — is what a *generator*
/// needs: what each node computes, and whether that survived the engine's
/// erasure well enough to be written back out as source ([`crate::codegen`]).
/// One projection feeds both, because both describe the same wired node.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NodeInfo {
    /// Position in the graph — the same index a
    /// [`Handle`](crate::interp::Handle) carries, and the one that appears in
    /// engine error messages ("node 3 (TryMap) cycle: ...").
    pub index: usize,
    /// The op kind: `type_name` shortened to its bare name (`Map`, `Fold`) for
    /// `#[op]` nodes, or the literal supplied by a hand-written one.
    pub label: String,
    /// How the engine activates this node beyond an upstream tick.
    pub activation: Activation,
    /// Upstreams whose tick activates this node.
    pub active_ups: Vec<usize>,
    /// Upstreams this node reads but is not triggered by.
    pub passive_ups: Vec<usize>,
    /// Tombstoned by a runtime removal (the `dynamic-graph` feature). Always
    /// `false` for a snapshot taken from a [`Builder`](crate::interp::Builder)
    /// and for any static graph. Removed nodes are omitted from every rendered
    /// format.
    pub removed: bool,
    /// What this node computes, verbatim, when the wiring quoted its closure
    /// with [`func!`](crate::func) or recorded it with
    /// [`#[wiring]`](crate::wiring). `None` for an unquoted closure — not
    /// "no closure": the engine erased it and no traversal can recover it.
    ///
    /// For a tier-2 quotation this is the **emittable** form — the body wrapped
    /// in a block that re-materialises each capture — not the bare body, which
    /// would only resolve where it was written.
    #[serde(default)]
    pub src: Option<String>,
    /// `(file, line)` of that quotation.
    #[serde(default)]
    pub loc: Option<(String, u32)>,
    /// The op's `#[op(build = …)]` method name (`"map"`) — what an emitter has
    /// to write, as opposed to [`label`](Self::label), which is the type name
    /// (`"Map"`) a human reads in an error. `None` for a hand-written node.
    #[serde(default)]
    pub build: Option<String>,
    /// The node's data config rendered as Rust source by
    /// [`EmitLiteral`](crate::emit::EmitLiteral), when the wiring recorded one
    /// with [`Stream::with_cfg`](crate::fluent::Stream::with_cfg).
    #[serde(default)]
    pub cfg_src: Option<String>,
    /// Whether this op takes a **closure** config. Read together with
    /// [`src`](Self::src): `takes_closure_cfg && src.is_none()` is the precise
    /// statement "this node has a closure the engine erased and the wiring did
    /// not quote" — the one an emitter must refuse on. Neither field alone says
    /// it, because a config-free op also reports `src: None`.
    #[serde(default)]
    pub takes_closure_cfg: bool,
    /// The op's passive-edge mask: bit `i` set means edge `i` of the original
    /// call is passive. Use it with
    /// [`edges_in_call_order`](Self::edges_in_call_order), which is what it
    /// exists for.
    #[serde(default)]
    pub passive_mask: u32,
    /// Whether the op takes a call-site **seed** (`fold(0u64, f)`) beside its
    /// `Cfg`, from `#[op(init_arg)]`.
    ///
    /// Read together with [`cfg_src`](Self::cfg_src): `has_init_arg &&
    /// cfg_src.is_none()` means the seed was never recorded, so emitting would
    /// drop it. Neither field alone says that — `Fold`'s `Cfg` is its closure,
    /// so [`takes_closure_cfg`](Self::takes_closure_cfg) is about the closure,
    /// not the seed.
    #[serde(default)]
    pub has_init_arg: bool,
    /// Names [`#[wiring]`](crate::wiring) detected as captures of this node's
    /// closure and could not render as source.
    ///
    /// Non-empty means the node is **not** emittable even though
    /// [`src`](Self::src) is `Some`: the recorded body refers to a binding that
    /// exists only in the wiring, so splicing it into an artifact would produce
    /// source that does not compile. `codegen::ineligible` reports these first,
    /// since "captures something unrenderable" is a different fix from "closure
    /// not quoted".
    #[serde(default)]
    pub unresolved_captures: Vec<String>,
}

impl Default for NodeInfo {
    fn default() -> Self {
        Self {
            index: 0,
            label: String::new(),
            activation: Activation::NONE,
            active_ups: Vec::new(),
            passive_ups: Vec::new(),
            removed: false,
            src: None,
            loc: None,
            build: None,
            cfg_src: None,
            takes_closure_cfg: false,
            passive_mask: 0,
            has_init_arg: false,
            unresolved_captures: Vec::new(),
        }
    }
}

impl NodeInfo {
    /// True if nothing upstream feeds this node — a source (`ticker`,
    /// `constant`, `external`, `channel`, `poll`, a `feedback` read end).
    pub fn is_source(&self) -> bool {
        self.active_ups.is_empty() && self.passive_ups.is_empty()
    }

    /// The node's upstream edges **in the order the original call listed
    /// them** — receiver first, then each argument.
    ///
    /// [`active_ups`](Self::active_ups) and [`passive_ups`](Self::passive_ups)
    /// are partitioned lists: each preserves its own order, but the
    /// interleaving between them is not recoverable from the pair alone.
    /// `sample`'s data leg is edge 0 and passive while its trigger is edge 1
    /// and active, so `active_ups = [trigger]`, `passive_ups = [data]` — and
    /// nothing there says the call was `data.sample(&trigger)` rather than the
    /// reverse. [`passive_mask`](Self::passive_mask) supplies exactly that
    /// missing bit, and this walks it.
    pub fn edges_in_call_order(&self) -> Vec<usize> {
        let total = self.active_ups.len() + self.passive_ups.len();
        let (mut active, mut passive) = (self.active_ups.iter(), self.passive_ups.iter());
        (0..total)
            .map(|i| {
                let from_passive = i < 32 && (self.passive_mask >> i) & 1 == 1;
                let next = if from_passive {
                    passive.next()
                } else {
                    active.next()
                };
                *next.expect(
                    "invariant: passive_mask agrees with the active/passive \
                     partition it was recorded alongside",
                )
            })
            .collect()
    }

    /// A short human tag for the activation: `"schedules"`, `"threaded"`,
    /// `"always"`, or `None` for [`Activation::NONE`] (activated purely by an
    /// upstream tick — the common case, not worth annotating).
    pub fn activation_tag(&self) -> Option<&'static str> {
        if self.activation.always {
            Some("always")
        } else if self.activation.threaded {
            Some("threaded")
        } else if self.activation.schedules {
            Some("schedules")
        } else {
            None
        }
    }
}

/// The wired topology of a graph, captured without running it.
///
/// See the [module docs](self) for what it is for and how to get one.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GraphSnapshot {
    /// Every node, in wiring order — which is a valid topological order over
    /// all edges, active and passive, because the fluent API forces a stream to
    /// exist before it can be referenced. `nodes[i].index == i`.
    pub nodes: Vec<NodeInfo>,
}

impl GraphSnapshot {
    /// Build a snapshot from raw parts. Crate-internal: the engine constructs
    /// these from its own node records.
    pub(crate) fn new(nodes: Vec<NodeInfo>) -> Self {
        Self { nodes }
    }

    /// Number of nodes, **including** any tombstoned by a runtime removal.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Every edge in the graph, ascending by downstream index then by
    /// upstream index, active edges before passive ones for a given
    /// downstream. Tombstoned nodes contribute none.
    pub fn edges(&self) -> Vec<Edge> {
        let mut out = Vec::new();
        for node in self.live() {
            for &from in &node.active_ups {
                if self.is_live(from) {
                    out.push(Edge {
                        from,
                        to: node.index,
                        kind: EdgeKind::Active,
                    });
                }
            }
            for &from in &node.passive_ups {
                if self.is_live(from) {
                    out.push(Edge {
                        from,
                        to: node.index,
                        kind: EdgeKind::Passive,
                    });
                }
            }
        }
        out
    }

    /// Number of edges, both kinds.
    pub fn edge_count(&self) -> usize {
        self.edges().len()
    }

    /// The nodes still in the graph — everything, unless a runtime removal
    /// tombstoned some.
    pub fn live(&self) -> impl Iterator<Item = &NodeInfo> {
        self.nodes.iter().filter(|n| !n.removed)
    }

    /// Nodes with no upstream at all: the graph's sources.
    pub fn sources(&self) -> impl Iterator<Item = &NodeInfo> {
        self.live().filter(|n| n.is_source())
    }

    /// Nodes nothing reads: the graph's sinks (its outputs and its side
    /// effects).
    ///
    /// An engine-produced snapshot always satisfies `nodes[i].index == i`, but
    /// these types are public and `Deserialize`, so a hand-built or
    /// round-tripped one need not — an out-of-range `index` is treated as
    /// having no downstream rather than panicking.
    pub fn sinks(&self) -> impl Iterator<Item = &NodeInfo> {
        let mut has_downstream = vec![false; self.nodes.len()];
        for edge in self.edges() {
            has_downstream[edge.from] = true;
        }
        self.live()
            .filter(move |n| !has_downstream.get(n.index).copied().unwrap_or(false))
    }

    fn is_live(&self, index: usize) -> bool {
        self.nodes.get(index).is_some_and(|n| !n.removed)
    }

    /// The node label as it is rendered in a picture: `[3] Map`, with the
    /// activation appended when there is one (`[0] Ticker (schedules)`).
    fn display_label(node: &NodeInfo) -> String {
        match node.activation_tag() {
            Some(tag) => format!("[{}] {} ({tag})", node.index, node.label),
            None => format!("[{}] {}", node.index, node.label),
        }
    }

    // -----------------------------------------------------------------------
    // Renderers
    // -----------------------------------------------------------------------

    /// Graphviz DOT. Pipe it to `dot -Tsvg` for the richest rendering of the
    /// four — it is the only format here that lays out a wide DAG well.
    ///
    /// Sources are filled, passive edges are dashed and grey, active edges are
    /// solid.
    ///
    /// ```
    /// use std::time::Duration;
    /// use wingfoil::prelude::*;
    ///
    /// let g = GraphBuilder::new();
    /// let _ = g.ticker(Duration::from_millis(1)).count();
    /// assert!(g.snapshot().to_dot().starts_with("digraph wingfoil {"));
    /// ```
    pub fn to_dot(&self) -> String {
        let mut out = String::new();
        out.push_str("digraph wingfoil {\n");
        out.push_str("    rankdir=LR;\n");
        out.push_str("    node [shape=box, style=rounded, fontname=\"Helvetica\"];\n");
        out.push_str("    edge [fontname=\"Helvetica\", fontsize=10];\n");
        for node in self.live() {
            let label = dot_escape(&Self::display_label(node));
            if node.is_source() {
                let _ = writeln!(
                    out,
                    "    n{} [label=\"{label}\", style=\"rounded,filled\", fillcolor=\"#e8f0fe\"];",
                    node.index
                );
            } else {
                let _ = writeln!(out, "    n{} [label=\"{label}\"];", node.index);
            }
        }
        for edge in self.edges() {
            match edge.kind {
                EdgeKind::Active => {
                    let _ = writeln!(out, "    n{} -> n{};", edge.from, edge.to);
                }
                EdgeKind::Passive => {
                    let _ = writeln!(
                        out,
                        "    n{} -> n{} [style=dashed, color=\"#888888\", \
                         arrowhead=empty, label=\"passive\"];",
                        edge.from, edge.to
                    );
                }
            }
        }
        out.push_str("}\n");
        out
    }

    /// Mermaid `flowchart`. GitHub renders this inline in a README, an issue or
    /// a pull request, so it is the format to reach for when the picture is
    /// going into a document rather than onto a screen.
    ///
    /// Active edges are solid (`-->`), passive edges dotted (`-.->`).
    pub fn to_mermaid(&self) -> String {
        let mut out = String::new();
        out.push_str("flowchart LR\n");
        for node in self.live() {
            let label = mermaid_escape(&Self::display_label(node));
            // Sources get the stadium shape, everything else a plain box.
            if node.is_source() {
                let _ = writeln!(out, "    n{}([\"{label}\"])", node.index);
            } else {
                let _ = writeln!(out, "    n{}[\"{label}\"]", node.index);
            }
        }
        for edge in self.edges() {
            match edge.kind {
                EdgeKind::Active => {
                    let _ = writeln!(out, "    n{} --> n{}", edge.from, edge.to);
                }
                EdgeKind::Passive => {
                    let _ = writeln!(out, "    n{} -.->|passive| n{}", edge.from, edge.to);
                }
            }
        }
        out
    }

    /// JSON, for a tool that wants to consume the topology rather than look at
    /// it. The shape matches what `serde` derives for these types, so a
    /// consumer can deserialize it straight back into a [`GraphSnapshot`].
    ///
    /// Written directly rather than through `serde_json` so the library does
    /// not take that dependency for a debug facility; the round trip is pinned
    /// by `tests/introspect.rs`.
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\"nodes\":[");
        for (i, node) in self.nodes.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "{{\"index\":{},\"label\":\"{}\",\"activation\":{{\"schedules\":{},\
                 \"threaded\":{},\"always\":{}}},\"active_ups\":{},\"passive_ups\":{},\
                 \"removed\":{},\"src\":{},\"loc\":{},\"build\":{},\"cfg_src\":{},\
                 \"takes_closure_cfg\":{},\"passive_mask\":{},\"has_init_arg\":{},\
                 \"unresolved_captures\":{}}}",
                node.index,
                json_escape(&node.label),
                node.activation.schedules,
                node.activation.threaded,
                node.activation.always,
                json_usize_array(&node.active_ups),
                json_usize_array(&node.passive_ups),
                node.removed,
                json_opt_str(node.src.as_deref()),
                json_opt_loc(node.loc.as_ref()),
                json_opt_str(node.build.as_deref()),
                json_opt_str(node.cfg_src.as_deref()),
                node.takes_closure_cfg,
                node.passive_mask,
                node.has_init_arg,
                json_str_array(&node.unresolved_captures),
            );
        }
        out.push_str("]}");
        out
    }

    /// GML, byte-compatible in structure with legacy wingfoil's
    /// `Graph::export` so existing GML tooling (yEd, Gephi) keeps working.
    ///
    /// Retained for continuity only — it cannot express the active/passive
    /// distinction, so [`to_dot`](Self::to_dot) or
    /// [`to_mermaid`](Self::to_mermaid) show strictly more.
    pub fn to_gml(&self) -> String {
        let mut out = String::from("graph [\n");
        for node in self.live() {
            let _ = writeln!(out, "    node [");
            let _ = writeln!(out, "        id {}", node.index);
            let _ = writeln!(
                out,
                "        label \"{}\"",
                dot_escape(&Self::display_label(node))
            );
            let _ = writeln!(out, "        graphics");
            let _ = writeln!(out, "        [");
            let _ = writeln!(out, "            w 200.0");
            let _ = writeln!(out, "            h 30.0");
            let _ = writeln!(out, "        ]");
            let _ = writeln!(out, "    ]");
        }
        for edge in self.edges() {
            let _ = writeln!(out, "    edge [");
            let _ = writeln!(out, "        source {}", edge.from);
            let _ = writeln!(out, "        target {}", edge.to);
            let _ = writeln!(out, "    ]");
        }
        out.push_str("]\n");
        out
    }

    /// A compact plain-text listing — one line per node, upstreams named. What
    /// you want from a test failure or a terminal, where a picture is no use.
    ///
    /// ```text
    /// wingfoil graph: 3 nodes, 2 edges
    ///   [0] Ticker (schedules)
    ///   [1] Count            <- 0
    ///   [2] Map              <- 1
    /// ```
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "wingfoil graph: {} nodes, {} edges",
            self.live().count(),
            self.edge_count()
        );
        for node in self.live() {
            let mut line = format!("  {}", Self::display_label(node));
            if !node.active_ups.is_empty() {
                let _ = write!(line, " <- {}", join_usize(&node.active_ups));
            }
            if !node.passive_ups.is_empty() {
                let _ = write!(line, " <~ {}", join_usize(&node.passive_ups));
            }
            let _ = writeln!(out, "{line}");
        }
        out
    }
}

/// `to_text` is the `Display` form, so `println!("{snapshot}")` does the
/// obvious thing.
impl std::fmt::Display for GraphSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_text())
    }
}

fn join_usize(xs: &[usize]) -> String {
    xs.iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A `null` or a quoted, escaped string — what serde writes for an
/// `Option<String>`, which [`GraphSnapshot::to_json`] has to match byte for
/// byte.
fn json_opt_str(s: Option<&str>) -> String {
    match s {
        Some(s) => format!("\"{}\"", json_escape(s)),
        None => "null".to_string(),
    }
}

/// A `null` or a two-element `[file, line]` array — serde's rendering of the
/// `Option<(String, u32)>` a node's wiring location is recorded as.
fn json_opt_loc(loc: Option<&(String, u32)>) -> String {
    match loc {
        Some((file, line)) => format!("[\"{}\",{line}]", json_escape(file)),
        None => "null".to_string(),
    }
}

/// A JSON array of quoted, escaped strings.
fn json_str_array(xs: &[String]) -> String {
    let mut out = String::from("[");
    for (i, x) in xs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "\"{}\"", json_escape(x));
    }
    out.push(']');
    out
}

fn json_usize_array(xs: &[usize]) -> String {
    let mut out = String::from("[");
    for (i, x) in xs.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(out, "{x}");
    }
    out.push(']');
    out
}

/// Escape for a DOT (and GML) double-quoted string: backslash and quote.
fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape for a Mermaid quoted label. Mermaid has no backslash escape, so a
/// quote is written as its HTML entity — the renderer substitutes it back.
fn mermaid_escape(s: &str) -> String {
    s.replace('"', "#quot;")
}

/// Escape for a JSON string: the two mandatory characters plus the control
/// range. Node labels come from `type_name` or a literal, so this is
/// belt-and-braces rather than a hot path.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(index: usize, label: &str, active: &[usize], passive: &[usize]) -> NodeInfo {
        NodeInfo {
            index,
            label: label.to_string(),
            active_ups: active.to_vec(),
            passive_ups: passive.to_vec(),
            ..NodeInfo::default()
        }
    }

    fn sample() -> GraphSnapshot {
        GraphSnapshot::new(vec![
            NodeInfo {
                activation: Activation::SCHEDULES,
                ..node(0, "Ticker", &[], &[])
            },
            node(1, "Count", &[0], &[]),
            node(2, "Sample", &[0], &[1]),
        ])
    }

    #[test]
    fn edges_separate_active_from_passive() {
        let edges = sample().edges();
        assert_eq!(
            edges,
            vec![
                Edge {
                    from: 0,
                    to: 1,
                    kind: EdgeKind::Active
                },
                Edge {
                    from: 0,
                    to: 2,
                    kind: EdgeKind::Active
                },
                Edge {
                    from: 1,
                    to: 2,
                    kind: EdgeKind::Passive
                },
            ]
        );
    }

    #[test]
    fn sources_and_sinks() {
        let snap = sample();
        assert_eq!(snap.sources().map(|n| n.index).collect::<Vec<_>>(), vec![0]);
        assert_eq!(snap.sinks().map(|n| n.index).collect::<Vec<_>>(), vec![2]);
    }

    #[test]
    fn activation_appears_in_the_label() {
        assert_eq!(
            GraphSnapshot::display_label(&sample().nodes[0]),
            "[0] Ticker (schedules)"
        );
        assert_eq!(
            GraphSnapshot::display_label(&sample().nodes[1]),
            "[1] Count"
        );
    }

    #[test]
    fn dot_dashes_passive_edges() {
        let dot = sample().to_dot();
        assert!(dot.contains("n0 -> n1;"), "{dot}");
        assert!(dot.contains("n1 -> n2 [style=dashed"), "{dot}");
        // The source is filled; the others are not.
        assert!(dot.contains("n0 [label=\"[0] Ticker (schedules)\", style=\"rounded,filled\""));
    }

    #[test]
    fn mermaid_dots_passive_edges() {
        let mermaid = sample().to_mermaid();
        assert!(mermaid.starts_with("flowchart LR\n"), "{mermaid}");
        assert!(
            mermaid.contains("n0([\"[0] Ticker (schedules)\"])"),
            "{mermaid}"
        );
        assert!(mermaid.contains("n0 --> n1"), "{mermaid}");
        assert!(mermaid.contains("n1 -.->|passive| n2"), "{mermaid}");
    }

    #[test]
    fn removed_nodes_are_omitted_everywhere() {
        let mut snap = sample();
        snap.nodes[1].removed = true;
        // The node itself, and both edges touching it, disappear.
        assert_eq!(snap.live().count(), 2);
        assert_eq!(
            snap.edges(),
            vec![Edge {
                from: 0,
                to: 2,
                kind: EdgeKind::Active
            }]
        );
        assert!(!snap.to_dot().contains("n1 "));
        assert!(!snap.to_mermaid().contains("n1["));
        assert!(!snap.to_gml().contains("id 1"));
        // ...but it is still *present* in the data, flagged.
        assert_eq!(snap.node_count(), 3);
        assert!(snap.to_json().contains("\"removed\":true"));
    }

    /// These types are public and `Deserialize`, so `nodes[i].index == i` is
    /// an invariant of *engine-produced* snapshots only. A consumer that
    /// round-trips one through JSON can hand back anything, and the accessors
    /// must not panic on it.
    #[test]
    fn accessors_tolerate_an_out_of_range_index() {
        let snap = GraphSnapshot::new(vec![node(7, "Map", &[], &[])]);
        assert_eq!(snap.sinks().map(|n| n.index).collect::<Vec<_>>(), vec![7]);
        assert_eq!(snap.edges(), vec![]);
        assert_eq!(snap.sources().count(), 1);
    }

    #[test]
    fn escaping() {
        assert_eq!(dot_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(mermaid_escape(r#"a"b"#), "a#quot;b");
        assert_eq!(json_escape("a\"b\nc"), "a\\\"b\\nc");
        assert_eq!(json_escape("\u{1}"), "\\u0001");
    }

    #[test]
    fn text_form_lists_both_edge_kinds() {
        let text = sample().to_text();
        assert!(
            text.starts_with("wingfoil graph: 3 nodes, 3 edges\n"),
            "{text}"
        );
        assert!(text.contains("[2] Sample <- 0 <~ 1"), "{text}");
    }
}

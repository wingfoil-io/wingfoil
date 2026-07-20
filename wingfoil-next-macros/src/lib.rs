//! The `graph!` macro: **a fluent wiring function in, both engines out**.
//!
//! The macro body is a single, *valid Rust* function written against the
//! fluent API: it takes the builder as a parameter, wires streams with
//! ordinary `let` chains, and returns its output stream(s). The macro parses
//! the function, derives the DAG from the method chains, and expands to a
//! module (named after the function) containing:
//!
//! - `wire(g)` — your function, **verbatim** (renamed `wire`), reusable as
//!   ordinary fluent wiring;
//! - `interpreted()` — the graph built through `wire`, returning a `Runner`
//!   plus a typed handle per output;
//! - `compiled(run_mode, run_for)` — a fully monomorphized runner derived
//!   from the same tokens: node state in locals, tick propagation as
//!   `bool`s, every `Op::cycle` call (closures included) visible to the
//!   compiler;
//! - `nested(g, inputs...)` — the whole graph mounted as a **single
//!   compiled node** (an "island") inside an interpreted graph under
//!   construction. One closure owns all inner state; the outer engine pays
//!   one dyn call per activation for the entire sub-graph. Inner schedules
//!   (tickers, delays) are demultiplexed through a private queue, with only
//!   the earliest forwarded to the outer kernel.
//!
//! ```ignore
//! wingfoil_next::graph! {
//!     pub fn evens_sum(g: &GraphBuilder) -> Stream<u64> {
//!         let count = g.ticker(PERIOD).count();
//!         let is_even = count.map(|i| i.is_multiple_of(2));
//!         count.filter(&is_even).fold(0u64, |acc, v| *acc += v)
//!     }
//! }
//!
//! let (mut runner, sum) = evens_sum::interpreted();
//! let (sum2,) = evens_sum::compiled(run_mode, run_for);
//! ```
//!
//! # `compiled()` vs `nested()` — the graph *is* the program vs a *component*
//!
//! Both are generated from the same tokens and call the same `Op::cycle`
//! code, so they are semantically identical. They differ only in **who owns
//! the run loop**:
//!
//! - **`compiled()` — the whole program.** A standalone function that owns
//!   its own `Kernel`, keeps every node's state
//!   in local variables, runs the entire cycle loop to completion, and
//!   returns the declared output values. It is the top-level driver; nothing
//!   else runs it. Because LLVM sees the whole graph as one function it fuses
//!   across node boundaries and constant-folds (the dense speed-up). The
//!   price of being the whole program is that it is a closed box: static
//!   topology, outputs-only (no runner, no observing internals), and no IO,
//!   feedback, or live inputs.
//!
//! - **`nested()` — a component.** The same graph packed into a **single
//!   node** mounted inside a larger interpreted graph. It does *not* own a
//!   run loop — the outer interpreted engine drives it once per activation,
//!   like any node — but inside that node the dispatch is the *same*
//!   monomorphized straight-line code `compiled()` emits, so the interior
//!   runs at compiled speed. Inner tickers/delays can't touch the outer
//!   kernel directly, so the island keeps a private
//!   `TimeQueue` and forwards only its earliest
//!   pending time outward; on activation it pops the inner callbacks now due
//!   into its own dirty flags (a mini `begin_cycle`) and runs the interior.
//!   Time stays globally consistent because the island reads the outer
//!   engine's clock rather than keeping its own. The cost is one dyn call per
//!   activation at the boundary; the gain is composability — the island is
//!   one node, so the graph *around* it can be dynamic, IO-driven, observed,
//!   or itself contain other islands.
//!
//! This is the "islands of static in a sea of dynamic" pattern: reach for
//! `compiled()` when the whole computation is static and self-driving (a
//! backtest kernel, a build-time pipeline — run it, read the outputs); reach
//! for `nested()` when a mostly-dynamic or IO-driven graph has a hot inner
//! sub-computation worth compiling.
//!
//! Which expansions you get is a syntactic tell: a **self-contained** graph
//! (sources inside, no stream parameters) emits `compiled()` **and**
//! `nested()` — it can run as a whole program *or* mount as a source-island.
//! An **input-taking** graph (`fn ema(g, price: &Stream<f64>) -> Stream<f64>`)
//! emits **only** `nested()` — it inherently needs its inputs fed, so it is a
//! component, never a standalone program.
//!
//! The function takes the builder (any name) as its first parameter, then
//! optionally input streams (`name: &Stream<T>`) — the graph's edges in,
//! usable as chain roots. It returns `Stream<T>` — or a tuple
//! `(Stream<A>, Stream<B>, ...)` with a matching tuple of bound names as
//! the tail expression. A graph with inputs cannot run standalone, so it
//! expands to `wire` + `nested` only, and must have exactly one output
//! (a composite is one node with one output); `nested` is likewise only
//! emitted for single-output graphs. A passively read input (sample's data
//! edge) does not activate the island. Supported chain
//! methods mirror the fluent API: sources `.ticker(period)` /
//! `.constant(value)` on the builder; combinators `.map(f)`,
//! `.filter(&cond)`, `.fold(init, f)`, `.sample(&trigger)`, `.merge(&other)`,
//! `.join(&other, f)`, `.delay(duration)`; statistics (on `f64` streams)
//! `.ewma(decay)` / `.ewma_per_tick(alpha)` / `.ewma_half_life(dur)`,
//! `.rolling_sum(window)`, `.rolling_mean(window)`; sugar `.count()` and
//! `.accumulate()`.
//!
//! Arbitrary non-wiring statements are allowed anywhere in the body —
//! computing configs, declaring locals that closures capture, helper calls.
//! They are re-emitted into `compiled()` in source order (and are already
//! part of `wire()`), so both engines see them. Two rules, both enforced at
//! compile time: the builder and stream names may only appear in wiring
//! statements (`let name = <chain>;`) and the tail — wiring hidden inside
//! other code would build nodes `compiled()` cannot see; and passthrough
//! bindings may not shadow one another — `compiled()` inlines closures after
//! all statements run, so a shadowed capture would let the engines disagree.
//!
//! Limitations (v1): wiring itself must be straight-line (no wiring inside
//! loops/conditionals — the DAG must be static); IO-edge sources and sinks
//! (`external`, `poll`, `for_each`) are not expressible — IO lives at the
//! fluent layer, feeding compiled islands through their inputs.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    Expr, Ident, ImplItem, ItemFn, ItemImpl, Pat, ReturnType, Stmt, Token, Type, parse_macro_input,
    parse_quote,
};

#[derive(Clone, Copy, PartialEq)]
enum OpKind {
    /// A stream parameter of the wiring fn — an edge into the graph, not an
    /// op. Only valid in the `nested` expansion, where its value and tick
    /// flag are read from the *outer* graph.
    Input,
    Ticker,
    Constant,
    Map,
    Filter,
    Fold,
    Sample,
    Merge,
    Join,
    Delay,
    Ewma,
    RollingSum,
    RollingMean,
}

/// The shape of an op's `Op::In` tuple — how the cycle call assembles its
/// inputs from upstream value/tick locals. Adding an op picks an existing
/// shape; a genuinely novel shape adds one arm to [`cycle_input`].
enum Inputs {
    /// `()` — sources (ticker/constant).
    Unit,
    /// `(&v0,)` — one input read by value (map/fold/sample-data/ewma/…).
    One,
    /// `(&v0, &v1)` — two inputs by value (filter/join).
    Two,
    /// `(&v0, t0)` — one input plus its tick flag (delay).
    OneTick,
    /// `((&v0, t0), (&v1, t1))` — two `(value, tick)` pairs (merge).
    TwoTickPairs,
}

/// How `node_decl` initialises a node's `__cfg` local (the non-closure config;
/// closure configs keep no local and go through `cycle_owned_cfg` instead).
enum CfgInit {
    /// No `__cfg` local.
    None,
    /// `let __cfg [: ty] = exprs[arg];`
    Expr {
        arg: usize,
        ty: Option<TokenStream2>,
    },
    /// `let __cfg = NanoTime::from(exprs[arg]);`
    NanoTimeFrom(usize),
}

/// How `node_decl` initialises a node's `__state` local.
enum StateInit {
    /// No `__state` local.
    None,
    /// `let __state = exprs[arg];` (fold's accumulator seed).
    Expr(usize),
    /// `let __state = <ty>::default();`
    Default(TokenStream2),
    /// `let __state: Option<ty> = None;` (ticker's last-scheduled-time).
    OptionNone(TokenStream2),
}

/// How `node_decl` seeds a node's `__v_<name>` output value slot — the value
/// a passive/sample/join reader sees *before* the node's first tick. This must
/// match what the interpreted engine seeds its slot with (`interp.rs`), or the
/// engines drift on pre-first-tick reads.
enum ValueSeed {
    /// `let mut v = Default::default();` — the output type's default.
    Default,
    /// `let mut v = Clone::clone(&state);` — a clone of the op's `__state`
    /// local (fold seeds its slot with `init`, matching `interp.rs` which does
    /// `new_slot(init.clone())`). Requires the op to keep a `__state` local.
    CloneState,
}

/// Which of an op's upstream `refs` are *active* (propagate ticks / appear in
/// the dispatch condition). Single-sourced here so the interpreted engine, the
/// compiled emission, and the nested emission cannot disagree — previously
/// Sample's passive data edge was a hard-coded special case in three places.
enum Edges {
    /// Every `ref` is an active upstream.
    AllActive,
    /// Only the `ref` at this index is active; the rest are passive (read by
    /// value but do not trigger). Sample is `OneActive(1)`: its data source
    /// (`refs[0]`) is passive, its trigger (`refs[1]`) is the sole active edge.
    OneActive(usize),
}

/// Everything the emitters need to know about an op, **single-sourced** in one
/// [`OpKind::info`] arm per op: its type, the boolean facts dispatch consults,
/// and the structural shapes `node_decl`/`cycle_input`/`op_type` build from.
/// Adding an op is one `info` arm (plus its parse arm and enum variant); the
/// named fields — spelled in full per arm, with no `..base` fill — make a
/// half-filled entry a compile error, not a silent gap.
struct OpInfo {
    /// The concrete (inference-holed) op type, e.g. `::ops::Map<_, _, _>`.
    op_type: TokenStream2,
    /// Can be activated by a kernel callback → dispatch needs a dirty check.
    callback_activated: bool,
    /// Has a `start` hook to run before the first cycle.
    has_start: bool,
    /// `start` receives the state local too (only the ticker's start does).
    state_in_start: bool,
    /// Index in `exprs` of a closure config inlined at the cycle call via
    /// `cycle_owned_cfg` (direct-argument closure inference).
    owned_closure: Option<usize>,
    /// Unit output (ticker) → no value slot to declare or store into.
    unit_output: bool,
    inputs: Inputs,
    cfg_init: CfgInit,
    state_init: StateInit,
    /// How the output value slot is seeded before the first tick.
    value_seed: ValueSeed,
    /// Which upstream `refs` are active (drive dispatch).
    edges: Edges,
}

impl OpInfo {
    /// Keeps a `__cfg` local passed to `cycle` by `&mut`.
    fn cfg_local(&self) -> bool {
        !matches!(self.cfg_init, CfgInit::None)
    }
    /// Keeps a `__state` local passed to `cycle` by `&mut`.
    fn state_local(&self) -> bool {
        !matches!(self.state_init, StateInit::None)
    }
}

impl OpKind {
    fn info(self) -> OpInfo {
        let nanotime = quote! { ::wingfoil_next::wingfoil::NanoTime };
        // Every field is spelled out per arm — no `..base` fill — so adding a
        // field (or a new op) that omits it is a compile error, not a silent
        // gap that never fires compiled/nested.
        match self {
            // A graph edge, not a real op: it declares no storage and is never
            // dispatched (the `nested` expansion reads its value/tick from the
            // outer graph). A benign all-false `info` keeps whole-graph scans
            // like the callback-activation check from special-casing it.
            OpKind::Input => OpInfo {
                op_type: quote! {},
                callback_activated: false,
                has_start: false,
                state_in_start: false,
                owned_closure: None,
                unit_output: false,
                inputs: Inputs::One,
                cfg_init: CfgInit::None,
                state_init: StateInit::None,
                value_seed: ValueSeed::Default,
                edges: Edges::AllActive,
            },
            OpKind::Ticker => OpInfo {
                op_type: quote! { ::wingfoil_next::ops::Ticker },
                callback_activated: true,
                has_start: true,
                state_in_start: true,
                owned_closure: None,
                unit_output: true,
                inputs: Inputs::Unit,
                cfg_init: CfgInit::NanoTimeFrom(0),
                state_init: StateInit::OptionNone(nanotime),
                value_seed: ValueSeed::Default,
                edges: Edges::AllActive,
            },
            OpKind::Constant => OpInfo {
                op_type: quote! { ::wingfoil_next::ops::Const<_> },
                callback_activated: true,
                has_start: true,
                state_in_start: false,
                owned_closure: None,
                unit_output: false,
                inputs: Inputs::Unit,
                cfg_init: CfgInit::Expr { arg: 0, ty: None },
                state_init: StateInit::None,
                value_seed: ValueSeed::Default,
                edges: Edges::AllActive,
            },
            OpKind::Map => OpInfo {
                op_type: quote! { ::wingfoil_next::ops::Map<_, _, _> },
                callback_activated: false,
                has_start: false,
                state_in_start: false,
                owned_closure: Some(0),
                unit_output: false,
                inputs: Inputs::One,
                cfg_init: CfgInit::None,
                state_init: StateInit::None,
                value_seed: ValueSeed::Default,
                edges: Edges::AllActive,
            },
            OpKind::Filter => OpInfo {
                op_type: quote! { ::wingfoil_next::ops::Filter<_> },
                callback_activated: false,
                has_start: false,
                state_in_start: false,
                owned_closure: None,
                unit_output: false,
                inputs: Inputs::Two,
                cfg_init: CfgInit::None,
                state_init: StateInit::None,
                value_seed: ValueSeed::Default,
                edges: Edges::AllActive,
            },
            OpKind::Fold => OpInfo {
                op_type: quote! { ::wingfoil_next::ops::Fold<_, _, _> },
                callback_activated: false,
                has_start: false,
                state_in_start: false,
                owned_closure: Some(1),
                unit_output: false,
                inputs: Inputs::One,
                cfg_init: CfgInit::None,
                state_init: StateInit::Expr(0),
                // Seed the slot with a clone of the accumulator (init), so a
                // passive/sample/join read before the first tick sees `init`,
                // not `Default` — matching interp.rs `new_slot(init.clone())`.
                value_seed: ValueSeed::CloneState,
                edges: Edges::AllActive,
            },
            OpKind::Sample => OpInfo {
                op_type: quote! { ::wingfoil_next::ops::Sample<_> },
                callback_activated: false,
                has_start: false,
                state_in_start: false,
                owned_closure: None,
                unit_output: false,
                inputs: Inputs::One,
                cfg_init: CfgInit::None,
                state_init: StateInit::None,
                value_seed: ValueSeed::Default,
                // refs = [data, trigger]; only the trigger is active.
                edges: Edges::OneActive(1),
            },
            OpKind::Merge => OpInfo {
                op_type: quote! { ::wingfoil_next::ops::Merge2<_> },
                callback_activated: false,
                has_start: false,
                state_in_start: false,
                owned_closure: None,
                unit_output: false,
                inputs: Inputs::TwoTickPairs,
                cfg_init: CfgInit::None,
                state_init: StateInit::None,
                value_seed: ValueSeed::Default,
                edges: Edges::AllActive,
            },
            OpKind::Join => OpInfo {
                op_type: quote! { ::wingfoil_next::ops::Join<_, _, _, _> },
                callback_activated: false,
                has_start: false,
                state_in_start: false,
                owned_closure: Some(0),
                unit_output: false,
                inputs: Inputs::Two,
                cfg_init: CfgInit::None,
                state_init: StateInit::None,
                value_seed: ValueSeed::Default,
                edges: Edges::AllActive,
            },
            OpKind::Delay => OpInfo {
                op_type: quote! { ::wingfoil_next::ops::Delay<_> },
                callback_activated: true,
                has_start: false,
                state_in_start: false,
                owned_closure: None,
                unit_output: false,
                inputs: Inputs::OneTick,
                cfg_init: CfgInit::NanoTimeFrom(0),
                state_init: StateInit::Default(quote! { ::wingfoil_next::ops::DelayState }),
                value_seed: ValueSeed::Default,
                edges: Edges::AllActive,
            },
            OpKind::Ewma => OpInfo {
                op_type: quote! { ::wingfoil_next::ops::Ewma },
                callback_activated: false,
                has_start: false,
                state_in_start: false,
                owned_closure: None,
                unit_output: false,
                inputs: Inputs::One,
                cfg_init: CfgInit::Expr {
                    arg: 0,
                    ty: Some(quote! { ::wingfoil_next::ops::EwmaDecay }),
                },
                state_init: StateInit::Default(quote! { ::wingfoil_next::ops::EwmaState }),
                value_seed: ValueSeed::Default,
                edges: Edges::AllActive,
            },
            OpKind::RollingSum => OpInfo {
                op_type: quote! { ::wingfoil_next::ops::RollingSum },
                callback_activated: false,
                has_start: false,
                state_in_start: false,
                owned_closure: None,
                unit_output: false,
                inputs: Inputs::One,
                cfg_init: CfgInit::Expr {
                    arg: 0,
                    ty: Some(quote! { usize }),
                },
                state_init: StateInit::Default(quote! { ::wingfoil_next::ops::RollingWindowState }),
                value_seed: ValueSeed::Default,
                edges: Edges::AllActive,
            },
            OpKind::RollingMean => OpInfo {
                op_type: quote! { ::wingfoil_next::ops::RollingMean },
                callback_activated: false,
                has_start: false,
                state_in_start: false,
                owned_closure: None,
                unit_output: false,
                inputs: Inputs::One,
                cfg_init: CfgInit::Expr {
                    arg: 0,
                    ty: Some(quote! { usize }),
                },
                state_init: StateInit::Default(quote! { ::wingfoil_next::ops::RollingWindowState }),
                value_seed: ValueSeed::Default,
                edges: Edges::AllActive,
            },
        }
    }
}

struct NodeDef {
    name: Ident,
    op: OpKind,
    /// Indices (into the node list) of upstream nodes, in op-argument order.
    refs: Vec<usize>,
    /// Non-node args (configs, closures), in op-argument order.
    exprs: Vec<Expr>,
}

impl NodeDef {
    /// Active upstream node indices (what propagates ticks to this node).
    /// The active/passive edge shape is single-sourced in [`OpInfo::edges`]
    /// (e.g. Sample's data edge is passive, its trigger active) so the three
    /// emission paths cannot disagree.
    fn active_ups(&self) -> Vec<usize> {
        match self.op.info().edges {
            Edges::AllActive => self.refs.clone(),
            Edges::OneActive(i) => vec![self.refs[i]],
        }
    }
}

struct GraphDef {
    /// The module name (= the wiring function's name).
    name: Ident,
    /// The wiring function, re-emitted verbatim as `wire` in the module.
    wire_fn: ItemFn,
    /// Whether the builder parameter is taken by reference.
    builder_by_ref: bool,
    /// Stream parameters after the builder — the graph's input edges — and
    /// their value types. Present only in the `wire` + `nested` expansions
    /// (a graph with inputs cannot run standalone).
    inputs: Vec<(Ident, Type)>,
    /// Output bindings (from the tail expression) and their value types
    /// (from the `Stream<T>` return type), in order.
    outs: Vec<(Ident, Type)>,
    nodes: Vec<NodeDef>,
    /// Non-wiring statements (config computation etc.), re-emitted into
    /// `compiled()` (and present verbatim in `wire()`). Each is paired with
    /// the number of nodes defined before it so `compiled()` can interleave
    /// them with node declarations in source order — a passthrough `let` that
    /// shadows an earlier one must not affect configs evaluated before it.
    passthrough: Vec<(usize, Stmt)>,
}

/// The root identifier of a method-call chain, if the expression is one.
fn chain_root(expr: &Expr) -> Option<&Ident> {
    let mut cur = expr;
    loop {
        match cur {
            Expr::MethodCall(mc) => cur = &mc.receiver,
            Expr::Path(p) => return p.path.get_ident(),
            _ => return None,
        }
    }
}

/// The first identifier in `tokens` that names the builder or a known stream,
/// if any — used to reject graph wiring hidden inside arbitrary (passthrough)
/// code, which `wire()` would execute but `compiled()` could not see. Returns
/// the offending [`Ident`] (with its span) so the error can name it precisely.
fn offending_graph_ident(
    tokens: TokenStream2,
    builder: &Ident,
    streams: &std::collections::HashMap<String, usize>,
) -> Option<Ident> {
    use proc_macro2::TokenTree;
    for tt in tokens {
        match tt {
            TokenTree::Ident(i) if i == *builder || streams.contains_key(&i.to_string()) => {
                return Some(i);
            }
            TokenTree::Group(gp) => {
                if let Some(found) = offending_graph_ident(gp.stream(), builder, streams) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

/// Collect the identifiers a pattern binds (`let (a, b) = ..` → a, b).
fn pattern_idents(pat: &Pat, out: &mut Vec<Ident>) {
    match pat {
        Pat::Ident(p) => {
            out.push(p.ident.clone());
            if let Some((_, sub)) = &p.subpat {
                pattern_idents(sub, out);
            }
        }
        Pat::Type(p) => pattern_idents(&p.pat, out),
        Pat::Reference(p) => pattern_idents(&p.pat, out),
        Pat::Paren(p) => pattern_idents(&p.pat, out),
        Pat::Tuple(p) => p.elems.iter().for_each(|e| pattern_idents(e, out)),
        Pat::Slice(p) => p.elems.iter().for_each(|e| pattern_idents(e, out)),
        Pat::TupleStruct(p) => p.elems.iter().for_each(|e| pattern_idents(e, out)),
        Pat::Struct(p) => p.fields.iter().for_each(|f| pattern_idents(&f.pat, out)),
        Pat::Or(p) => p.cases.iter().for_each(|c| pattern_idents(c, out)),
        _ => {}
    }
}

/// Extract `T` from a `Stream<T>`-shaped type (any path ending in
/// `Stream<T>`).
fn stream_value_type(ty: &Type) -> syn::Result<Type> {
    if let Type::Path(p) = ty
        && let Some(seg) = p.path.segments.last()
        && seg.ident == "Stream"
        && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && args.args.len() == 1
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        return Ok(inner.clone());
    }
    Err(syn::Error::new(
        ty.span(),
        "graph functions must return `Stream<T>` (or a tuple of `Stream<T>`s)",
    ))
}

/// Prefix for macro-generated intermediate node names. User stream bindings
/// starting with it are rejected (see [`ChainWalker::walk_chain`]), so a
/// generated name can never collide with a user identifier in the emission.
const RESERVED_PREFIX: &str = "__wf_anon";

/// Builds the node list while walking fluent chains.
struct ChainWalker {
    nodes: Vec<NodeDef>,
    index_of: std::collections::HashMap<String, usize>,
    anon: usize,
}

impl ChainWalker {
    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            index_of: std::collections::HashMap::new(),
            anon: 0,
        }
    }

    fn push(&mut self, name: Ident, op: OpKind, refs: Vec<usize>, exprs: Vec<Expr>) -> usize {
        let ix = self.nodes.len();
        self.nodes.push(NodeDef {
            name,
            op,
            refs,
            exprs,
        });
        ix
    }

    fn anon_name(&mut self, span: proc_macro2::Span) -> Ident {
        self.anon += 1;
        // `RESERVED_PREFIX` (rejected for user bindings in `walk_chain`) makes
        // these intermediate node names un-collidable with user stream names —
        // a user stream `anon1` no longer shares the emitted `__v_anon1` slot
        // with a generated intermediate.
        Ident::new(&format!("{RESERVED_PREFIX}{}", self.anon), span)
    }

    fn lookup(&self, ident: &Ident) -> syn::Result<usize> {
        self.index_of
            .get(&ident.to_string())
            .copied()
            .ok_or_else(|| {
                syn::Error::new(
                    ident.span(),
                    format!("`{ident}` is not a stream bound by an earlier `let` in this graph"),
                )
            })
    }

    /// An argument referencing another stream: `&name` for a stream bound
    /// by a `let`, or bare `name` for an input parameter (which is already
    /// a `&Stream<T>`, so re-borrowing it would be a needless borrow).
    fn stream_ref_arg(&self, arg: &Expr) -> syn::Result<usize> {
        if let Expr::Reference(r) = arg
            && let Expr::Path(p) = &*r.expr
            && let Some(ident) = p.path.get_ident()
        {
            return self.lookup(ident);
        }
        if let Expr::Path(p) = arg
            && let Some(ident) = p.path.get_ident()
        {
            return self.lookup(ident);
        }
        Err(syn::Error::new(
            arg.span(),
            "expected `&name` (or a bare input parameter name) referencing a stream",
        ))
    }

    /// Flatten one `let name = <chain>;` statement into nodes.
    fn walk_statement(
        &mut self,
        bound_name: &Ident,
        expr: &Expr,
        builder: &Ident,
    ) -> syn::Result<()> {
        // Collect the chain: innermost receiver first.
        let mut calls: Vec<(&Ident, Vec<&Expr>)> = Vec::new();
        let mut cur = expr;
        loop {
            match cur {
                Expr::MethodCall(mc) => {
                    calls.push((&mc.method, mc.args.iter().collect()));
                    cur = &mc.receiver;
                }
                Expr::Path(p) => {
                    let Some(root) = p.path.get_ident() else {
                        return Err(syn::Error::new(
                            p.span(),
                            "chain root must be the builder or a bound stream name",
                        ));
                    };
                    calls.reverse();
                    return self.walk_chain(bound_name, root, builder, &calls, expr.span());
                }
                other => {
                    return Err(syn::Error::new(
                        other.span(),
                        "expected a fluent method chain rooted at the builder or a bound stream",
                    ));
                }
            }
        }
    }

    fn walk_chain(
        &mut self,
        bound_name: &Ident,
        root: &Ident,
        builder: &Ident,
        calls: &[(&Ident, Vec<&Expr>)],
        span: proc_macro2::Span,
    ) -> syn::Result<()> {
        let mut calls = calls.iter().peekable();

        // Resolve the chain's starting node: either a source call on the
        // builder, or a previously bound stream.
        let mut cur: usize = if root == builder {
            let Some((method, args)) = calls.next() else {
                return Err(syn::Error::new(
                    span,
                    "the builder must be followed by a source call",
                ));
            };
            let name = if calls.peek().is_none() {
                bound_name.clone()
            } else {
                self.anon_name(method.span())
            };
            match method.to_string().as_str() {
                "ticker" | "constant" => {
                    let op = if *method == "ticker" {
                        OpKind::Ticker
                    } else {
                        OpKind::Constant
                    };
                    expect_arity(method, args, 1)?;
                    self.push(name, op, vec![], vec![args[0].clone()])
                }
                other => {
                    return Err(syn::Error::new(
                        method.span(),
                        format!(
                            "unknown source `.{other}(..)` on the builder; expected `.ticker(..)` \
                             or `.constant(..)` (external sources are not supported in graph! — \
                             use the fluent API directly)"
                        ),
                    ));
                }
            }
        } else {
            self.lookup(root)?
        };

        // Each further method call appends a node consuming `cur`.
        while let Some((method, args)) = calls.next() {
            let name = if calls.peek().is_none() {
                bound_name.clone()
            } else {
                self.anon_name(method.span())
            };
            cur = match method.to_string().as_str() {
                "map" => {
                    expect_arity(method, args, 1)?;
                    self.push(name, OpKind::Map, vec![cur], vec![args[0].clone()])
                }
                "fold" => {
                    expect_arity(method, args, 2)?;
                    self.push(
                        name,
                        OpKind::Fold,
                        vec![cur],
                        vec![args[0].clone(), args[1].clone()],
                    )
                }
                "filter" => {
                    expect_arity(method, args, 1)?;
                    let cond = self.stream_ref_arg(args[0])?;
                    self.push(name, OpKind::Filter, vec![cur, cond], vec![])
                }
                "sample" => {
                    expect_arity(method, args, 1)?;
                    let trigger = self.stream_ref_arg(args[0])?;
                    self.push(name, OpKind::Sample, vec![cur, trigger], vec![])
                }
                "merge" => {
                    expect_arity(method, args, 1)?;
                    let other = self.stream_ref_arg(args[0])?;
                    self.push(name, OpKind::Merge, vec![cur, other], vec![])
                }
                "join" => {
                    expect_arity(method, args, 2)?;
                    let other = self.stream_ref_arg(args[0])?;
                    self.push(name, OpKind::Join, vec![cur, other], vec![args[1].clone()])
                }
                "delay" => {
                    expect_arity(method, args, 1)?;
                    self.push(name, OpKind::Delay, vec![cur], vec![args[0].clone()])
                }
                // Sugar, desugared to the same folds the fluent layer uses.
                "count" => {
                    expect_arity(method, args, 0)?;
                    let init: Expr = parse_quote!(0u64);
                    let f: Expr = parse_quote!(|__acc, _| *__acc += 1);
                    self.push(name, OpKind::Fold, vec![cur], vec![init, f])
                }
                "accumulate" => {
                    expect_arity(method, args, 0)?;
                    let init: Expr = parse_quote!(::std::vec::Vec::new());
                    let f: Expr =
                        parse_quote!(|__acc, __v| __acc.push(::core::clone::Clone::clone(__v)));
                    self.push(name, OpKind::Fold, vec![cur], vec![init, f])
                }
                // Statistics ops (f64 → f64). The three `ewma*` spellings share
                // one op, differing only in the `EwmaDecay` config they build.
                "ewma" => {
                    expect_arity(method, args, 1)?;
                    self.push(name, OpKind::Ewma, vec![cur], vec![args[0].clone()])
                }
                "ewma_per_tick" => {
                    expect_arity(method, args, 1)?;
                    let alpha = args[0];
                    let cfg: Expr = parse_quote!(::wingfoil_next::ops::EwmaDecay::PerTick(#alpha));
                    self.push(name, OpKind::Ewma, vec![cur], vec![cfg])
                }
                "ewma_half_life" => {
                    expect_arity(method, args, 1)?;
                    let half_life = args[0];
                    let cfg: Expr = parse_quote!(::wingfoil_next::ops::EwmaDecay::HalfLife(
                        (#half_life).as_nanos() as f64
                    ));
                    self.push(name, OpKind::Ewma, vec![cur], vec![cfg])
                }
                "rolling_sum" => {
                    expect_arity(method, args, 1)?;
                    self.push(name, OpKind::RollingSum, vec![cur], vec![args[0].clone()])
                }
                "rolling_mean" => {
                    expect_arity(method, args, 1)?;
                    self.push(name, OpKind::RollingMean, vec![cur], vec![args[0].clone()])
                }
                other => {
                    return Err(syn::Error::new(
                        method.span(),
                        format!(
                            "unknown combinator `.{other}(..)`; expected one of: map, filter, \
                             fold, sample, merge, join, delay, count, accumulate, ewma, \
                             ewma_per_tick, ewma_half_life, rolling_sum, rolling_mean"
                        ),
                    ));
                }
            };
        }

        if bound_name.to_string().starts_with(RESERVED_PREFIX) {
            return Err(syn::Error::new(
                bound_name.span(),
                format!(
                    "`{bound_name}` uses the reserved `{RESERVED_PREFIX}` prefix — it is used for \
                     macro-generated intermediate node names; pick another name"
                ),
            ));
        }
        if self.index_of.insert(bound_name.to_string(), cur).is_some() {
            return Err(syn::Error::new(
                bound_name.span(),
                format!("`{bound_name}` is bound twice"),
            ));
        }
        Ok(())
    }
}

fn expect_arity(method: &Ident, args: &[&Expr], want: usize) -> syn::Result<()> {
    if args.len() == want {
        Ok(())
    } else {
        Err(syn::Error::new(
            method.span(),
            format!(
                "`.{method}(..)` takes {want} argument(s), got {}",
                args.len()
            ),
        ))
    }
}

impl Parse for GraphDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let wire_fn: ItemFn = input.parse()?;
        if !input.is_empty() {
            return Err(input.error("graph! takes exactly one function definition"));
        }
        let sig = &wire_fn.sig;
        if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
            return Err(syn::Error::new(
                sig.generics.span(),
                "graph functions cannot be generic",
            ));
        }
        if sig.asyncness.is_some() || sig.constness.is_some() || sig.unsafety.is_some() {
            return Err(syn::Error::new(
                sig.span(),
                "graph functions must be plain `fn`s",
            ));
        }

        // The first parameter is the builder; its name roots source chains.
        // Any further parameters are input streams (`name: &Stream<T>`) —
        // the graph's edges in, roots for combinator chains.
        let mut params = sig.inputs.iter();
        let Some(syn::FnArg::Typed(builder_param)) = params.next() else {
            return Err(syn::Error::new(
                sig.inputs.span(),
                "graph functions take the builder as their first parameter, e.g. \
                 `g: &GraphBuilder`",
            ));
        };
        let Pat::Ident(builder_pat) = &*builder_param.pat else {
            return Err(syn::Error::new(
                builder_param.pat.span(),
                "the builder parameter must be a plain name",
            ));
        };
        let builder = builder_pat.ident.clone();
        let builder_by_ref = matches!(&*builder_param.ty, Type::Reference(_));

        let mut inputs: Vec<(Ident, Type)> = Vec::new();
        for param in params {
            let syn::FnArg::Typed(param) = param else {
                return Err(syn::Error::new(
                    param.span(),
                    "graph functions take no self",
                ));
            };
            let Pat::Ident(pat) = &*param.pat else {
                return Err(syn::Error::new(
                    param.pat.span(),
                    "stream parameters must be plain names",
                ));
            };
            let Type::Reference(reference) = &*param.ty else {
                return Err(syn::Error::new(
                    param.ty.span(),
                    "stream parameters must be taken by reference: `name: &Stream<T>`",
                ));
            };
            let value_ty = stream_value_type(&reference.elem)?;
            inputs.push((pat.ident.clone(), value_ty));
        }

        // Output value types from the return type.
        let ReturnType::Type(_, ret_ty) = &sig.output else {
            return Err(syn::Error::new(
                sig.span(),
                "graph functions must return `Stream<T>` (or a tuple of `Stream<T>`s)",
            ));
        };
        let out_types: Vec<Type> = match &**ret_ty {
            Type::Tuple(t) => t
                .elems
                .iter()
                .map(stream_value_type)
                .collect::<syn::Result<_>>()?,
            other => vec![stream_value_type(other)?],
        };

        // Walk the body. A `let` whose initialiser is a method chain rooted
        // at the builder or a known stream is graph wiring; every other
        // statement is arbitrary passthrough code (config computation,
        // helpers) — re-emitted into both expansions — provided it does not
        // touch graph identifiers (wiring hidden in arbitrary code would
        // build nodes `compiled()` cannot see).
        let mut walker = ChainWalker::new();
        // Input streams are pseudo-nodes: chains may root at them, and the
        // `nested` expansion reads their value/tick from the outer graph.
        for (name, _) in &inputs {
            let ix = walker.push(name.clone(), OpKind::Input, vec![], vec![]);
            walker.index_of.insert(name.to_string(), ix);
        }
        let mut passthrough: Vec<(usize, Stmt)> = Vec::new();
        let mut passthrough_names = std::collections::HashSet::new();
        let mut tail: Option<&Expr> = None;
        let n_stmts = wire_fn.block.stmts.len();
        for (i, stmt) in wire_fn.block.stmts.iter().enumerate() {
            // Is this a wiring statement?
            if let Stmt::Local(local) = stmt
                && let Some(init) = &local.init
                && let Some(root) = chain_root(&init.expr)
                && (*root == builder || walker.index_of.contains_key(&root.to_string()))
            {
                let Pat::Ident(pat) = &local.pat else {
                    return Err(syn::Error::new(
                        local.pat.span(),
                        "bind each stream chain to a plain name",
                    ));
                };
                walker.walk_statement(&pat.ident, &init.expr, &builder)?;
                continue;
            }
            // The tail expression names the returned stream binding(s).
            if let Stmt::Expr(expr, None) = stmt
                && i + 1 == n_stmts
            {
                tail = Some(expr);
                continue;
            }
            // Passthrough: arbitrary non-wiring code.
            let tokens = quote! { #stmt };
            if let Some(bad) = offending_graph_ident(tokens, &builder, &walker.index_of) {
                return Err(syn::Error::new(
                    bad.span(),
                    format!(
                        "`{bad}` (the builder or a bound stream name) may only appear in \
                         straight-line wiring `let name = <chain>;` statements, not inside other \
                         code — compiled() could not see nodes built there"
                    ),
                ));
            }
            if let Stmt::Local(local) = stmt {
                let mut bound = Vec::new();
                pattern_idents(&local.pat, &mut bound);
                for ident in bound {
                    if !passthrough_names.insert(ident.to_string()) {
                        return Err(syn::Error::new(
                            ident.span(),
                            format!(
                                "`{ident}` shadows an earlier binding — shadowing is not \
                                 supported inside graph! (compiled() inlines closures after \
                                 all statements run, so the engines would disagree on which \
                                 binding a closure captures)"
                            ),
                        ));
                    }
                }
            }
            passthrough.push((walker.nodes.len(), stmt.clone()));
        }
        let Some(tail) = tail else {
            return Err(syn::Error::new(
                wire_fn.block.span(),
                "the body must end with the returned stream name(s), e.g. `sum` or `(a, b)`",
            ));
        };
        let out_names: Vec<Ident> = match tail {
            Expr::Path(p) if p.path.get_ident().is_some() => {
                vec![p.path.get_ident().cloned().expect("checked above")]
            }
            Expr::Tuple(t) => t
                .elems
                .iter()
                .map(|e| {
                    if let Expr::Path(p) = e
                        && let Some(ident) = p.path.get_ident()
                    {
                        Ok(ident.clone())
                    } else {
                        Err(syn::Error::new(
                            e.span(),
                            "tail tuple elements must be bound stream names",
                        ))
                    }
                })
                .collect::<syn::Result<_>>()?,
            other => {
                return Err(syn::Error::new(
                    other.span(),
                    "the tail expression must be a bound stream name or a tuple of them",
                ));
            }
        };
        if out_names.len() != out_types.len() {
            return Err(syn::Error::new(
                tail.span(),
                format!(
                    "the return type declares {} output stream(s) but the tail names {}",
                    out_types.len(),
                    out_names.len()
                ),
            ));
        }
        // Resolve each tail name to the *actual node* it names, then use that
        // node's own name for the value/tick slots. A bare alias (`let out =
        // acc;`) makes `out` an alias for `acc`'s node with no node of its own,
        // so `__v_out` would be undefined compiled/nested — resolving to the
        // node name (`acc`) makes it reference the real slot in all three paths.
        let mut resolved_out_names: Vec<Ident> = Vec::with_capacity(out_names.len());
        for name in &out_names {
            let Some(&ix) = walker.index_of.get(&name.to_string()) else {
                return Err(syn::Error::new(
                    name.span(),
                    format!("`{name}` is not a stream bound by a `let` in this body"),
                ));
            };
            if walker.nodes[ix].op == OpKind::Input {
                return Err(syn::Error::new(
                    name.span(),
                    format!(
                        "`{name}` is an input parameter — outputs must be streams built \
                             in this body"
                    ),
                ));
            }
            resolved_out_names.push(walker.nodes[ix].name.clone());
        }
        let out_names = resolved_out_names;
        if !inputs.is_empty() && out_names.len() != 1 {
            return Err(syn::Error::new(
                tail.span(),
                "a graph with input streams must return exactly one `Stream<T>` — it expands \
                 to a single `nested` node, which has one output",
            ));
        }

        Ok(GraphDef {
            name: sig.ident.clone(),
            builder_by_ref,
            inputs,
            outs: out_names.into_iter().zip(out_types).collect(),
            nodes: walker.nodes,
            passthrough,
            wire_fn,
        })
    }
}

/// See the crate docs: fluent wiring in, `interpreted()` + `compiled()` out.
#[proc_macro]
pub fn graph(input: TokenStream) -> TokenStream {
    let def = parse_macro_input!(input as GraphDef);
    let out = expand(&def);
    if std::env::var_os("GRAPH_DEBUG").is_some() {
        eprintln!("=== graph! {} ===\n{}", def.name, out);
    }
    out.into()
}

/// `#[op(build = <name>)]` — attribute on an `impl Op for X` block that also
/// emits the interpreted engine's `Builder::<name>` wiring method, so an op's
/// semantics and its interpreted entry point are single-sourced.
///
/// Scoped to the **single-active-input** shape: `type In<'a> = (&'a I,)`,
/// engine-owned `Cfg`/`State` with `State: Default`, and no lifecycle hooks
/// (the common case — `map`, `distinct`, `ewma`, …). The generated method is a
/// thin wrapper over [`Builder::register_op1`], with the node label derived
/// from `type_name::<X>()` rather than a hand-written string. Ops that don't
/// fit (multiple inputs, passive edges, tick-flag inputs, sources, custom
/// state seeds, lifecycle hooks) keep their hand-written `Builder` methods.
#[proc_macro_attribute]
pub fn op(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as OpArgs);
    let imp = parse_macro_input!(item as ItemImpl);
    match expand_op(&args, &imp) {
        Ok(extra) => quote! { #imp #extra }.into(),
        Err(e) => {
            let e = e.to_compile_error();
            quote! { #imp #e }.into()
        }
    }
}

/// Parsed `#[op(build = <ident>)]` arguments.
struct OpArgs {
    build: Ident,
}

impl Parse for OpArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        if key != "build" {
            return Err(syn::Error::new(
                key.span(),
                "expected `build = <method name>`",
            ));
        }
        input.parse::<Token![=]>()?;
        let build: Ident = input.parse()?;
        Ok(OpArgs { build })
    }
}

/// The `T` of a single-element reference tuple type `(&'a T,)`, or an error if
/// the type is not that shape (which is how `#[op]` enforces its scope).
fn single_ref_tuple_elem(ty: &Type) -> syn::Result<Type> {
    if let Type::Tuple(tup) = ty
        && tup.elems.len() == 1
        && let Type::Reference(r) = &tup.elems[0]
    {
        return Ok((*r.elem).clone());
    }
    Err(syn::Error::new(
        ty.span(),
        "#[op] supports only single-input ops: `type In<'a>` must be `(&'a T,)`",
    ))
}

/// True for the unit type `()` — a `Cfg = ()` op takes no config argument.
fn is_unit_type(ty: &Type) -> bool {
    matches!(ty, Type::Tuple(t) if t.elems.is_empty())
}

fn expand_op(args: &OpArgs, imp: &ItemImpl) -> syn::Result<TokenStream2> {
    let self_ty = &*imp.self_ty;

    // Pull `In` / `Cfg` / `Out` off the impl's associated types.
    let (mut in_ty, mut cfg_ty, mut out_ty) = (None, None, None);
    for it in &imp.items {
        if let ImplItem::Type(t) = it {
            match t.ident.to_string().as_str() {
                "In" => in_ty = Some(t.ty.clone()),
                "Cfg" => cfg_ty = Some(t.ty.clone()),
                "Out" => out_ty = Some(t.ty.clone()),
                _ => {}
            }
        }
    }
    let missing =
        |what: &str| syn::Error::new(imp.span(), format!("#[op]: impl has no `type {what}`"));
    let in_ty = in_ty.ok_or_else(|| missing("In"))?;
    let cfg_ty = cfg_ty.ok_or_else(|| missing("Cfg"))?;
    let out_ty = out_ty.ok_or_else(|| missing("Out"))?;
    let input_ty = single_ref_tuple_elem(&in_ty)?;

    let name = &args.build;
    let has_cfg = !is_unit_type(&cfg_ty);
    let cfg_param = if has_cfg {
        quote! { , cfg: #cfg_ty }
    } else {
        quote! {}
    };
    let cfg_arg = if has_cfg {
        quote! { cfg }
    } else {
        quote! { () }
    };

    // Emit the impl's generic *parameters* bare (`<A, B, F>`) and route every
    // bound — inline ones and the impl's own `where` — into a single `where`
    // clause, so a param is never bounded in two places (`multiple_bound_locations`).
    // Add the two the slot machinery needs: an indexable input (`'static`) and
    // a default-able output slot.
    let mut bare_params: Vec<TokenStream2> = Vec::new();
    let mut preds: Vec<TokenStream2> = Vec::new();
    for p in &imp.generics.params {
        match p {
            syn::GenericParam::Type(tp) => {
                let id = &tp.ident;
                bare_params.push(quote! { #id });
                if !tp.bounds.is_empty() {
                    let bounds = &tp.bounds;
                    preds.push(quote! { #id: #bounds });
                }
            }
            other => bare_params.push(quote! { #other }),
        }
    }
    if let Some(w) = &imp.generics.where_clause {
        for pr in &w.predicates {
            preds.push(quote! { #pr });
        }
    }
    preds.push(quote! { #input_ty: 'static });
    preds.push(quote! { #out_ty: ::core::default::Default });
    let generics = if bare_params.is_empty() {
        quote! {}
    } else {
        quote! { <#(#bare_params),*> }
    };
    let where_tokens = quote! { where #(#preds),* };

    let doc = format!("Interpreted-engine wiring for [`{name}`]. Generated by `#[op]`.");
    Ok(quote! {
        impl crate::interp::Builder {
            #[doc = #doc]
            pub fn #name #generics (
                &mut self,
                src: crate::interp::Handle<#input_ty>
                #cfg_param
            ) -> crate::interp::Handle<#out_ty>
            #where_tokens
            {
                self.register_op1(
                    src,
                    ::core::any::type_name::<#self_ty>(),
                    <#self_ty as crate::op::Op>::ACTIVATION,
                    #cfg_arg,
                    ::core::default::Default::default(),
                    |__cfg, __state, __a, __ctx| {
                        <#self_ty as crate::op::Op>::cycle(__cfg, __state, (__a,), __ctx)
                    },
                )
            }
        }
    })
}

fn expand(def: &GraphDef) -> TokenStream2 {
    // A graph with input streams cannot run standalone — it expands to
    // `wire` + `nested` only. A self-contained graph gets all four.
    let standalone = if def.inputs.is_empty() {
        let interpreted = expand_interpreted(def);
        let compiled = expand_compiled(def);
        quote! { #interpreted #compiled }
    } else {
        quote! {}
    };
    let nested = if def.outs.len() == 1 {
        expand_nested(def)
    } else {
        // A multi-output graph is not nestable (a composite is one node
        // with one output); parsing guarantees this case has no inputs.
        quote! {}
    };
    let vis = &def.wire_fn.vis;
    let name = &def.name;
    // The user's function, verbatim, renamed `wire` and made pub within the
    // module — reusable as ordinary fluent wiring.
    let mut wire_fn = def.wire_fn.clone();
    wire_fn.sig.ident = Ident::new("wire", def.wire_fn.sig.ident.span());
    wire_fn.vis = parse_quote!(pub);
    // An input-only graph's wire fn never touches the builder parameter.
    wire_fn.attrs.push(parse_quote!(#[allow(unused_variables)]));
    quote! {
        #vis mod #name {
            #![allow(clippy::redundant_closure_call, clippy::let_and_return)]
            use super::*;
            use ::wingfoil_next::fluent::*;
            use ::wingfoil_next::stats::*;
            #wire_fn
            #standalone
            #nested
        }
    }
}

fn expand_interpreted(def: &GraphDef) -> TokenStream2 {
    let out_types: Vec<&Type> = def.outs.iter().map(|(_, t)| t).collect();
    let out_names: Vec<&Ident> = def.outs.iter().map(|(n, _)| n).collect();
    let call = if def.builder_by_ref {
        quote! { wire(&__g) }
    } else {
        quote! { wire(__g.clone()) }
    };
    let bind = if out_names.len() == 1 {
        let o = out_names[0];
        quote! { let #o = #call; }
    } else {
        quote! { let (#(#out_names),*) = #call; }
    };
    quote! {
        /// The graph built through [`wire`] — the function as written.
        /// Returns the runner plus a typed handle per output stream.
        pub fn interpreted() -> (
            ::wingfoil_next::interp::Runner,
            #(::wingfoil_next::interp::Handle<#out_types>,)*
        ) {
            let __g = ::wingfoil_next::fluent::GraphBuilder::new();
            #bind
            let __runner = __g.build();
            (__runner, #(#out_names.handle(),)*)
        }
    }
}

/// Which monomorphized expansion a per-node snippet is emitted into. The
/// two differ only in where an op's engine context comes from: `compiled`
/// owns the kernel; `nested` runs inside a composite closure whose
/// schedules land in a private queue (`__q`) keyed by inner node index.
#[derive(Clone, Copy)]
enum Target {
    Compiled,
    Nested,
}

fn ctx_expr(target: Target, idx: usize) -> TokenStream2 {
    match target {
        Target::Compiled => quote! { ::wingfoil_next::op::Ctx::new(&mut __k, #idx) },
        Target::Nested => {
            quote! { ::wingfoil_next::op::Ctx::nested(__now, __start_time, &mut __q, #idx) }
        }
    }
}

/// cfg + state + value slot declarations for one node, driven by the op's
/// [`OpInfo`] (`cfg_init` / `state_init` / `unit_output`).
fn node_decl(node: &NodeDef) -> TokenStream2 {
    // Inputs have no storage here — their value/tick are read from the outer
    // graph in the `nested` prologue.
    if node.op == OpKind::Input {
        return quote! {};
    }
    let name = &node.name;
    let cfg = format_ident!("__cfg_{}", name);
    let state = format_ident!("__state_{}", name);
    let value = format_ident!("__v_{}", name);
    let exprs = &node.exprs;
    let info = node.op.info();

    let cfg_decl = match &info.cfg_init {
        CfgInit::None => quote! {},
        CfgInit::Expr { arg, ty } => {
            let e = &exprs[*arg];
            match ty {
                Some(t) => quote! { let mut #cfg: #t = #e; },
                None => quote! { let mut #cfg = #e; },
            }
        }
        CfgInit::NanoTimeFrom(arg) => {
            let e = &exprs[*arg];
            quote! { let mut #cfg = ::wingfoil_next::wingfoil::NanoTime::from(#e); }
        }
    };
    let state_decl = match &info.state_init {
        StateInit::None => quote! {},
        StateInit::Expr(arg) => {
            let e = &exprs[*arg];
            quote! { let mut #state = #e; }
        }
        StateInit::Default(ty) => quote! { let mut #state = #ty::default(); },
        StateInit::OptionNone(ty) => quote! { let mut #state: Option<#ty> = None; },
    };
    // A **non-literal** closure config (map/fold/join) — e.g. `make_f()` or a
    // named local — is evaluated **once** here into a setup local, not inline
    // in the cycle loop: otherwise its factory re-runs every due cycle
    // (drifting from the interpreted engine, which evaluates it once at
    // wiring). The cycle then calls the ordinary `Op::cycle(&mut __cfg, …)`
    // (its concrete factory type needs no inference help). A *literal* closure
    // (`|i| ...`, `move |i| ...`) is left inline and passed by value directly
    // through `cycle_owned_cfg`: hoisting it to a `let` would strip the context
    // rustc needs to infer its argument types (E0282), and its construction is
    // a zero-cost ZST each cycle anyway.
    let closure_cfg_decl = match info.owned_closure {
        Some(ix) if !matches!(&exprs[ix], Expr::Closure(_)) => {
            let e = &exprs[ix];
            quote! { let mut #cfg = #e; }
        }
        _ => quote! {},
    };
    // A unit-output op (ticker) has no value slot to declare. Otherwise seed
    // the slot per the op's `value_seed`: `Default` for most, or a clone of
    // the `__state` local for fold (so pre-first-tick reads see `init`).
    let value_decl = if info.unit_output {
        quote! {}
    } else {
        match info.value_seed {
            ValueSeed::Default => quote! { let mut #value = Default::default(); },
            ValueSeed::CloneState => {
                quote! { let mut #value = ::core::clone::Clone::clone(&#state); }
            }
        }
    };
    quote! { #cfg_decl #closure_cfg_decl #state_decl #value_decl }
}

/// Passthrough statements interleaved with node declarations in source
/// order: a config expression must see exactly the passthrough bindings
/// that preceded its wiring statement in `wire()` (shadowing included).
fn interleaved_setup(def: &GraphDef) -> Vec<TokenStream2> {
    let mut setup: Vec<TokenStream2> = Vec::new();
    let mut passthrough = def.passthrough.iter().peekable();
    for (i, node) in def.nodes.iter().enumerate() {
        while let Some((_, stmt)) = passthrough.next_if(|(pos, _)| *pos <= i) {
            setup.push(quote! { #stmt });
        }
        setup.push(node_decl(node));
    }
    for (_, stmt) in passthrough {
        setup.push(quote! { #stmt });
    }
    setup
}

/// Which nodes need a named `__t_x` tick flag (someone consumes it) rather
/// than a bare `if`.
fn tick_flags_needed(def: &GraphDef) -> Vec<bool> {
    let mut has_active_downstream = vec![false; def.nodes.len()];
    for node in &def.nodes {
        for u in node.active_ups() {
            has_active_downstream[u] = true;
        }
    }
    def.nodes
        .iter()
        .enumerate()
        .map(|(i, node)| has_active_downstream[i] || def.outs.iter().any(|(o, _)| o == &node.name))
        .collect()
}

fn node_start(target: Target, idx: usize, node: &NodeDef) -> TokenStream2 {
    let cfg = format_ident!("__cfg_{}", node.name);
    let state = format_ident!("__state_{}", node.name);
    let op_path = op_path(node.op);
    let state_arg = start_state_arg(node, &state);
    let ctx = ctx_expr(target, idx);
    quote! {
        {
            let mut __ctx = #ctx;
            #op_path::start(&mut #cfg, #state_arg, &mut __ctx)?;
        }
    }
}

/// The dispatch line for one node: condition, `Op::cycle` call, tick flag.
fn node_dispatch(def: &GraphDef, target: Target, i: usize, needs_flag: bool) -> TokenStream2 {
    let node = &def.nodes[i];
    let name = &node.name;
    let cfg = format_ident!("__cfg_{}", name);
    let state = format_ident!("__state_{}", name);
    let value = format_ident!("__v_{}", name);
    let ticked = format_ident!("__t_{}", name);
    let idx = i;

    let mut cond_parts: Vec<TokenStream2> = node
        .active_ups()
        .iter()
        .map(|&u| {
            let t = format_ident!("__t_{}", def.nodes[u].name);
            quote! { #t }
        })
        .collect();
    if node.op.info().callback_activated {
        cond_parts.push(quote! { __dirty[#idx] });
    }
    let cond = quote! { #(#cond_parts)||* };

    let op_path = op_path(node.op);
    let input = cycle_input(def, node);
    let cfg_arg = cycle_cfg_arg(node, &cfg);
    let state_arg = cycle_state_arg(node, &state);
    let on_value = if node.op.info().unit_output {
        // Unit output: no value slot to store.
        quote! { ::wingfoil_next::op::Tick::Value(()) => true, }
    } else {
        quote! {
            ::wingfoil_next::op::Tick::Value(__val) => {
                #value = __val;
                true
            }
        }
    };
    // A *literal* closure config goes by value as a DIRECT argument so rustc
    // defers its signature inference until `input` has resolved the op's value
    // types (a closure behind `&mut` loses that deferral and fails E0282); it
    // is a zero-cost ZST rebuilt each cycle. A *non-literal* config (`make_f()`,
    // a named local) was evaluated once into `__cfg_<name>` by `node_decl` and
    // is passed here via the ordinary `Op::cycle(&mut __cfg, …)` — its concrete
    // type needs no inference help — so the factory runs once, not per cycle.
    let cycle_call = match node.op.info().owned_closure {
        Some(ix) => {
            if matches!(&node.exprs[ix], Expr::Closure(_)) {
                let f = &node.exprs[ix];
                let ty = op_type(node.op);
                quote! {
                    ::wingfoil_next::op::cycle_owned_cfg::<#ty>(#f, #state_arg, #input, &mut __ctx)
                }
            } else {
                quote! { #op_path::cycle(&mut #cfg, #state_arg, #input, &mut __ctx) }
            }
        }
        None => quote! { #op_path::cycle(#cfg_arg, #state_arg, #input, &mut __ctx) },
    };
    let ctx = ctx_expr(target, idx);
    // `?` propagates an op error out of the enclosing `compiled()` fn or the
    // `nested()` composite closure — both return `anyhow::Result`.
    let call = quote! {
        {
            let mut __ctx = #ctx;
            match #cycle_call? {
                #on_value
                ::wingfoil_next::op::Tick::Quiet => false,
            }
        }
    };

    if needs_flag {
        quote! { let #ticked = (#cond) && #call; let _ = #ticked; }
    } else {
        quote! { if #cond { let _ = #call; } }
    }
}

fn expand_compiled(def: &GraphDef) -> TokenStream2 {
    let n = def.nodes.len();
    let flags = tick_flags_needed(def);
    let setup = interleaved_setup(def);
    let mut starts = Vec::new();
    let mut body = Vec::new();
    for (i, node) in def.nodes.iter().enumerate() {
        if node.op.info().has_start {
            starts.push(node_start(Target::Compiled, i, node));
        }
        body.push(node_dispatch(def, Target::Compiled, i, flags[i]));
    }

    let out_types: Vec<&Type> = def.outs.iter().map(|(_, t)| t).collect();
    let out_values: Vec<TokenStream2> = def
        .outs
        .iter()
        .map(|(o, _)| output_value_expr(def, o))
        .collect();

    quote! {
        /// The same graph, fully monomorphized: node state in locals, tick
        /// propagation as bools, every `Op::cycle` (closures included)
        /// visible to the compiler. Derived from the same tokens as
        /// `interpreted`, so the two cannot drift.
        pub fn compiled(
            run_mode: ::wingfoil_next::wingfoil::RunMode,
            run_for: ::wingfoil_next::wingfoil::RunFor,
        ) -> ::wingfoil_next::anyhow::Result<( #(#out_types,)* )> {
            let mut __k = ::wingfoil_next::wingfoil::codegen::Kernel::new(run_mode, run_for);
            #(#setup)*
            #(#starts)*
            let mut __dirty = [false; #n];
            while __k.begin_cycle(&mut __dirty) {
                #(#body)*
                __k.end_cycle(&mut __dirty);
            }
            Ok(( #(#out_values,)* ))
        }
    }
}

/// The whole graph as a single node ("compiled island") mounted in an
/// interpreted graph. One closure owns all inner state; inner dispatch is
/// the same monomorphized straight-line code `compiled` emits, with inner
/// schedules demultiplexed through a private `TimeQueue` — only the
/// earliest is forwarded to the outer kernel.
fn expand_nested(def: &GraphDef) -> TokenStream2 {
    let n = def.nodes.len();
    let (out_name, out_ty) = &def.outs[0];
    let out_t = format_ident!("__t_{}", out_name);
    let out_v = output_value_expr(def, out_name);
    let callback_activated = def
        .nodes
        .iter()
        .any(|node| node.op.info().callback_activated);
    let flags = tick_flags_needed(def);
    let setup = interleaved_setup(def);
    let mut starts = Vec::new();
    let mut body = Vec::new();
    for (i, node) in def.nodes.iter().enumerate() {
        if node.op == OpKind::Input {
            continue;
        }
        if node.op.info().has_start {
            starts.push(node_start(Target::Nested, i, node));
        }
        body.push(node_dispatch(def, Target::Nested, i, flags[i]));
    }

    // Only inputs that are an *active* upstream of some inner node activate
    // the composite; a passively read input (sample's data edge) must not.
    let mut input_active = vec![false; n];
    for node in &def.nodes {
        for u in node.active_ups() {
            if def.nodes[u].op == OpKind::Input {
                input_active[u] = true;
            }
        }
    }

    let in_names: Vec<&Ident> = def.inputs.iter().map(|(name, _)| name).collect();
    let in_tys: Vec<&Type> = def.inputs.iter().map(|(_, ty)| ty).collect();
    let slot_ids: Vec<Ident> = in_names
        .iter()
        .map(|name| format_ident!("__slot_{}", name))
        .collect();
    let ix_ids: Vec<Ident> = in_names
        .iter()
        .map(|name| format_ident!("__ix_{}", name))
        .collect();
    let t_ids: Vec<Ident> = in_names
        .iter()
        .map(|name| format_ident!("__t_{}", name))
        .collect();
    let v_ids: Vec<Ident> = in_names
        .iter()
        .map(|name| format_ident!("__v_{}", name))
        .collect();
    let active_ix_ids: Vec<&Ident> = def
        .inputs
        .iter()
        .enumerate()
        .filter(|(i, _)| input_active[*i])
        .map(|(i, _)| &ix_ids[i])
        .collect();
    let ticked_bind = if def.inputs.is_empty() {
        quote! {}
    } else {
        quote! { let __ticked = __g.__ticked(); }
    };

    quote! {
        /// The whole graph mounted as a **single compiled node** in an
        /// interpreted graph under construction. The outer engine pays one
        /// dyn call per activation for the entire sub-graph; inside, state
        /// lives in one closure and dispatch is monomorphized straight-line
        /// code — the same code `compiled` emits. Inner schedules (tickers,
        /// delays) are demultiplexed through a private queue; only the
        /// earliest is forwarded to the outer kernel.
        #[allow(unused_mut, unused_variables)]
        pub fn nested(
            __g: &::wingfoil_next::fluent::GraphBuilder,
            #(#in_names: &::wingfoil_next::fluent::Stream<#in_tys>,)*
        ) -> ::wingfoil_next::fluent::Stream<#out_ty> {
            #(#setup)*
            #ticked_bind
            #(
                let #slot_ids = #in_names.__slot();
                let #ix_ids = #in_names.handle().index();
            )*
            let mut __q = ::wingfoil_next::wingfoil::TimeQueue::<usize>::new();
            let mut __dirty = [false; #n];
            let __active: ::std::vec::Vec<usize> =
                ::std::vec::Vec::from([#(#active_ix_ids),*]);
            __g.__composite(__active, #callback_activated, move |__ctx, __is_start| {
                let __now = __ctx.time();
                let __start_time = __ctx.start_time();
                if __is_start {
                    #(#starts)*
                    if let Some(__t) = __q.next_time() {
                        __ctx.schedule(__t);
                    }
                    return ::core::result::Result::Ok(::wingfoil_next::op::Tick::Quiet);
                }
                for __d in __dirty.iter_mut() {
                    *__d = false;
                }
                while let Some(__ix) = __q.pop_if_pending(__now) {
                    __dirty[__ix] = true;
                }
                #(
                    let #t_ids = __ticked.borrow()[#ix_ids];
                    let #v_ids = #slot_ids.borrow();
                )*
                #(#body)*
                if let Some(__t) = __q.next_time() {
                    __ctx.schedule(__t);
                }
                ::core::result::Result::Ok(if #out_t {
                    ::wingfoil_next::op::Tick::Value(::core::clone::Clone::clone(&#out_v))
                } else {
                    ::wingfoil_next::op::Tick::Quiet
                })
            })
        }
    }
}

/// The concrete (inference-holed) op type, e.g. `::ops::Map<_, _, _>`.
fn op_type(op: OpKind) -> TokenStream2 {
    op.info().op_type
}

/// The value expression for an output node (`o` is a resolved node name): its
/// `__v_<name>` slot, or the unit literal `()` for a unit-output op (a bare
/// ticker returned directly), which declares no value slot.
fn output_value_expr(def: &GraphDef, o: &Ident) -> TokenStream2 {
    let is_unit = def
        .nodes
        .iter()
        .find(|n| &n.name == o)
        .is_some_and(|n| n.op.info().unit_output);
    if is_unit {
        quote! { () }
    } else {
        let v = format_ident!("__v_{}", o);
        quote! { #v }
    }
}

/// The op type wrapped as `<Type as Op>` so trait items resolve without
/// `Op` being in scope at the expansion site.
fn op_path(op: OpKind) -> TokenStream2 {
    let ty = op_type(op);
    quote! { <#ty as ::wingfoil_next::op::Op> }
}

fn cycle_cfg_arg(node: &NodeDef, cfg: &Ident) -> TokenStream2 {
    if node.op.info().cfg_local() {
        quote! { &mut #cfg }
    } else {
        quote! { &mut () }
    }
}

fn cycle_state_arg(node: &NodeDef, state: &Ident) -> TokenStream2 {
    if node.op.info().state_local() {
        quote! { &mut #state }
    } else {
        quote! { &mut () }
    }
}

fn start_state_arg(node: &NodeDef, state: &Ident) -> TokenStream2 {
    if node.op.info().state_in_start {
        quote! { &mut #state }
    } else {
        quote! { &mut () }
    }
}

/// The `Op::In` tuple for one cycle, built from upstream value/tick locals.
/// A ticker upstream has no value slot — its value is the unit literal. An
/// input upstream's local is a borrow guard on the outer graph's slot, so
/// it is re-borrowed rather than referenced.
fn cycle_input(def: &GraphDef, node: &NodeDef) -> TokenStream2 {
    let v = |ix: usize| -> TokenStream2 {
        let up = &def.nodes[ix];
        let ident = format_ident!("__v_{}", up.name);
        match up.op {
            OpKind::Ticker => quote! { &() },
            OpKind::Input => quote! { &*#ident },
            _ => quote! { &#ident },
        }
    };
    let t = |ix: usize| format_ident!("__t_{}", def.nodes[ix].name);
    match node.op.info().inputs {
        Inputs::Unit => quote! { () },
        Inputs::One => {
            let src = v(node.refs[0]);
            quote! { (#src,) }
        }
        Inputs::Two => {
            let (a, b) = (v(node.refs[0]), v(node.refs[1]));
            quote! { (#a, #b) }
        }
        Inputs::OneTick => {
            let src = v(node.refs[0]);
            let ts = t(node.refs[0]);
            quote! { (#src, #ts) }
        }
        Inputs::TwoTickPairs => {
            let (a, b) = (v(node.refs[0]), v(node.refs[1]));
            let (ta, tb) = (t(node.refs[0]), t(node.refs[1]));
            quote! { ((#a, #ta), (#b, #tb)) }
        }
    }
}

//! The `nitro!` macro: **a fluent wiring function in, both engines out**.
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
//! wingfoil_next::nitro! {
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
//! edge) does not activate the island.
//!
//! **The op set is open.** The macro has no per-op table: every method call —
//! built-in catalog op or user-defined — is dispatched through one mechanism,
//! the naming-convention forwarder functions (`__wf_op_<name>_cycle` /
//! `_cycle_owned` / `_start` / `_start_owned` / `_seed_state` / `_seed_value`,
//! plus the `__WF_OP_<NAME>_ACTIVATION` / `_PASSIVE` consts) that `#[op]`
//! generates next to the op's impl. The macro never names an op type: rustc's
//! inference resolves it from the argument types, and the consts fold into
//! the dispatch conditions after monomorphization. Call arguments classify
//! at expansion time: `&name` naming a bound stream → an input edge (active,
//! read by value, in call order after the receiver); a literal closure → the
//! op's by-value config; anything else → the plain config (one value or a
//! tuple). The macro knows exactly **two** method names of its own — the
//! topology combinators `.map_n(N, f)` / `.fan(N, |s| <sub-chain>)`, which
//! create N nodes (inexpressible per-node) and whose counts must be integer
//! literals so the unrolled DAG stays static — the static-topology answer
//! to "no wiring inside loops" below. Everything else, `.count()` and
//! `.ewma_per_tick(..)` included, is an ordinary op.
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
//! loops/conditionals — the DAG must be static; use `.map_n` / `.fan` for
//! regular repeated topologies); IO-edge sources and sinks
//! (`external`, `poll`, `for_each`) are not expressible — IO lives at the
//! fluent layer, feeding compiled islands through their inputs.

use proc_macro::TokenStream;
use proc_macro2::Span;
use proc_macro2::TokenStream as TokenStream2;
use proc_macro2::TokenTree;
use quote::{ToTokens, format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    Attribute, Expr, Ident, ImplItem, ItemFn, ItemImpl, LitStr, Pat, ReturnType, Stmt, Token, Type,
    Visibility, braced, parse_macro_input, parse_quote,
};

/// One node of the parsed DAG. There is **one emission mechanism**: every op
/// is dispatched through naming-convention forwarder functions
/// (`__wf_op_<method>_cycle` / `_cycle_owned` / `_start` / `_start_owned` /
/// `_seed_state` / `_seed_value`, plus the `__WF_OP_<METHOD>_ACTIVATION` and
/// `__WF_OP_<METHOD>_PASSIVE` consts), generated by `#[op]` next to the op's
/// impl — or hand-written for exotic shapes (fold's cfg-derived seeds,
/// sample's passive edge). The macro never names an op type: rustc's
/// inference resolves it from the argument types at the expansion site, and
/// per-op consts fold into the dispatch conditions after monomorphization.
///
/// Call-site classification (in `apply_call` / the source arm):
/// - an argument `&name` (or a bare input-parameter `name`) where `name` is
///   a stream bound in this graph → an **edge** (`refs`, receiver first);
/// - a literal closure → `closure`, passed by value at the cycle call (the
///   `cycle_owned_cfg` inference-deferral trick);
/// - anything else → `plain`, the op's `Cfg` (one value, or a tuple).
struct NodeDef {
    name: Ident,
    /// The op's method name, naming its forwarder family. `None` for input
    /// pseudo-nodes (stream parameters of the wiring fn), which have no
    /// storage or dispatch — the `nested` expansion reads their value/tick
    /// from the outer graph.
    method: Option<Ident>,
    /// Upstream node indices: the receiver first (combinators), then every
    /// `&stream` argument in call order. Empty for sources.
    refs: Vec<usize>,
    /// Non-stream, non-closure arguments, in call order — the op's `Cfg`.
    plain: Vec<Expr>,
    /// At most one literal-closure argument.
    closure: Option<Expr>,
    /// The op takes its edges as a **pair slice** (`&[(value, tick), ..]`)
    /// rather than a fixed-arity tuple, so it accepts a fan-in of any width
    /// from one signature. Set only by the fan-in arms of `apply_call`
    /// (`fan` / `merge_all`, both of which emit the variadic `merge_n`); every
    /// other node keeps the uniform tuple input. A variadic op is all-active
    /// by construction, so its dispatch condition skips the passive mask —
    /// which also keeps the mask's `>> position` shift inside `u32` when the
    /// fan-in is wider than 32.
    variadic: bool,
}

impl NodeDef {
    fn is_input(&self) -> bool {
        self.method.is_none()
    }

    fn method(&self) -> &Ident {
        self.method
            .as_ref()
            .expect("invariant: input pseudo-nodes are skipped before emission")
    }

    /// The forwarder function ident `__wf_op_<method>_<kind>`, spanned at the
    /// call-site method so an op with no forwarders errors *there*.
    fn forwarder(&self, kind: &str) -> Ident {
        let method = self.method();
        Ident::new(&format!("__wf_op_{method}_{kind}"), method.span())
    }

    /// The `__WF_OP_<METHOD>_<kind>` const ident (ACTIVATION / PASSIVE).
    fn op_const(&self, kind: &str) -> Ident {
        let method = self.method();
        Ident::new(
            &format!("__WF_OP_{}_{kind}", method.to_string().to_uppercase()),
            method.span(),
        )
    }

    /// Whether this node keeps a `__cfg_<name>` local (any plain args).
    fn has_cfg(&self) -> bool {
        !self.plain.is_empty()
    }

    /// The plain-config expression: one value, or a tuple in call order.
    fn cfg_expr(&self) -> TokenStream2 {
        match self.plain.len() {
            0 => quote! { () },
            1 => {
                let e = &self.plain[0];
                quote! { #e }
            }
            _ => {
                let es = &self.plain;
                quote! { ( #(#es),* ) }
            }
        }
    }
}

struct NitroDef {
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
        "nitro functions must return `Stream<T>` (or a tuple of `Stream<T>`s)",
    ))
}

/// Prefix for macro-generated intermediate node names. User stream bindings
/// starting with it are rejected (see [`ChainWalker::walk_chain`]), so a
/// generated name can never collide with a user identifier in the emission.
/// No leading underscore: the emission concatenates it after `__v_`/`__t_`/…,
/// and a leading `_` would produce a triple-underscore (non-snake-case) local.
const RESERVED_PREFIX: &str = "wf_anon_";

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

    /// Push a node for op `method`, classifying `args`: `&stream` → edge,
    /// literal closure → by-value cycle config, anything else → plain `Cfg`.
    fn push_op(
        &mut self,
        name: Ident,
        method: Ident,
        receiver: Option<usize>,
        args: &[&Expr],
    ) -> syn::Result<usize> {
        let mut refs: Vec<usize> = receiver.into_iter().collect();
        let mut plain: Vec<Expr> = Vec::new();
        let mut closure: Option<Expr> = None;
        for arg in args {
            if let Ok(edge) = self.stream_ref_arg(arg) {
                refs.push(edge);
            } else if matches!(arg, Expr::Closure(_)) {
                if closure.is_some() {
                    return Err(syn::Error::new(
                        method.span(),
                        format!(
                            "`.{method}(..)` has more than one literal closure argument; ops take \
                             at most one (bind extras to a `let` first)"
                        ),
                    ));
                }
                closure = Some((*arg).clone());
            } else {
                plain.push((*arg).clone());
            }
        }
        Ok(self.push_node(name, Some(method), refs, plain, closure))
    }

    fn push_node(
        &mut self,
        name: Ident,
        method: Option<Ident>,
        refs: Vec<usize>,
        plain: Vec<Expr>,
        closure: Option<Expr>,
    ) -> usize {
        let ix = self.nodes.len();
        self.nodes.push(NodeDef {
            name,
            method,
            refs,
            plain,
            closure,
            variadic: false,
        });
        ix
    }

    /// Push the single n-ary `merge_n` node that fans `tails` back in, in
    /// supplied order (so the tie-break stays "earliest supplied that ticked
    /// wins"). One tail needs no merge at all — it *is* the result.
    fn push_merge_n(&mut self, name: Ident, span: proc_macro2::Span, tails: Vec<usize>) -> usize {
        if let [only] = tails[..] {
            return only;
        }
        let ix = self.push_node(name, Some(Ident::new("merge_n", span)), tails, vec![], None);
        self.nodes[ix].variadic = true;
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
            // Sources go through the same forwarder mechanism as combinators
            // — `.ticker(..)`, `.constant(..)`, or any user source with
            // `#[op]`-generated forwarders. No receiver, so no edges (a
            // `&stream` argument still classifies as an edge, which a
            // source's forwarder arity will reject).
            self.push_op(name, (*method).clone(), None, args)?
        } else {
            self.lookup(root)?
        };

        // Each further method call appends node(s) consuming `cur`. Most
        // combinators push a single node; `map_n` / `fan` expand to several
        // (bounded compile-time repetition — the DAG stays static).
        while let Some(call) = calls.next() {
            let (method, args) = (call.0, &call.1);
            let name = if calls.peek().is_none() {
                bound_name.clone()
            } else {
                self.anon_name(method.span())
            };
            cur = self.apply_call(cur, method, args, name)?;
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

    /// Dispatch one fluent method call onto tail `cur`, pushing the node(s)
    /// it expands to and returning the new tail. Exactly **two** names are
    /// known to the macro — `map_n` and `fan`, the topology combinators
    /// (they create N nodes, which no per-node mechanism can express, and
    /// their counts must be literals so the DAG stays static). Every other
    /// method — built-in or user-defined — is one op node dispatched
    /// through its forwarders.
    fn apply_call(
        &mut self,
        cur: usize,
        method: &Ident,
        args: &[&Expr],
        name: Ident,
    ) -> syn::Result<usize> {
        Ok(match method.to_string().as_str() {
            // Bounded repetition sugar. `map_n(N, f)` unrolls to N chained
            // `map`s; `fan(N, |s| <sub-chain>)` builds N copies of a sub-chain
            // rooted at the current tail and merges them. Both take a literal
            // count so the emitted DAG stays static.
            "map_n" => {
                expect_arity(method, args, 2)?;
                let n = parse_static_count(method, args[0])?;
                let f = args[1];
                let map = Ident::new("map", method.span());
                let mut tail = cur;
                for k in 0..n {
                    let node_name = if k + 1 == n {
                        name.clone()
                    } else {
                        self.anon_name(method.span())
                    };
                    tail = self.push_op(node_name, map.clone(), Some(tail), &[f])?;
                }
                tail
            }
            "fan" => {
                expect_arity(method, args, 2)?;
                let n = parse_static_count(method, args[0])?;
                if n == 0 {
                    return Err(syn::Error::new(
                        args[0].span(),
                        "`.fan(0, ..)` requires at least one branch",
                    ));
                }
                let Expr::Closure(closure) = args[1] else {
                    return Err(syn::Error::new(
                        args[1].span(),
                        "`.fan(n, |s| ..)` second argument must be a closure taking the \
                         per-branch source stream",
                    ));
                };
                if closure.inputs.len() != 1 {
                    return Err(syn::Error::new(
                        closure.span(),
                        "the `.fan` closure must take exactly one parameter (the per-branch \
                         source stream)",
                    ));
                }
                let mut params = Vec::new();
                pattern_idents(&closure.inputs[0], &mut params);
                let Some(param) = params.first().cloned() else {
                    return Err(syn::Error::new(
                        closure.inputs[0].span(),
                        "the `.fan` closure parameter must be a simple name",
                    ));
                };
                let tails: Vec<usize> = (0..n)
                    .map(|_| self.walk_template(&param, cur, &closure.body))
                    .collect::<syn::Result<_>>()?;
                // Fan the branch tails back in through one n-ary merge, as the
                // fluent `fan` does — "first supplied that ticked wins", and a
                // 256-way fan stays 1 merge node deep instead of 255.
                self.push_merge_n(name, method.span(), tails)
            }
            // The n-ary merge, spelled as the fluent method: its argument is a
            // slice *literal* of stream references, which the generic arm
            // cannot classify (a `&[..]` expression is not a `&stream` edge),
            // so its elements are destructured into edges here.
            "merge_all" => {
                expect_arity(method, args, 1)?;
                let elems = match args[0] {
                    Expr::Reference(r) => match &*r.expr {
                        Expr::Array(a) => &a.elems,
                        _ => return Err(merge_all_arg_error(args[0])),
                    },
                    Expr::Array(a) => &a.elems,
                    _ => return Err(merge_all_arg_error(args[0])),
                };
                let mut tails = vec![cur];
                for elem in elems {
                    tails.push(self.stream_ref_arg(elem)?);
                }
                self.push_merge_n(name, method.span(), tails)
            }
            // Everything else — built-in or user op — dispatches through its
            // naming-convention forwarders. A typo (or an op with no
            // forwarders in scope) fails as an unresolved
            // `__wf_op_<name>_cycle`, spanned at this method.
            _ => self.push_op(name, method.clone(), Some(cur), args)?,
        })
    }

    /// Walk a `fan` sub-chain template: a fluent chain rooted at the closure
    /// parameter `param`, which resolves to node `root` (the fan source).
    /// Returns the tail node index without binding a user name — every node is
    /// anonymous, since the branch tails are consumed by the merge.
    fn walk_template(&mut self, param: &Ident, root: usize, body: &Expr) -> syn::Result<usize> {
        let mut calls: Vec<(&Ident, Vec<&Expr>)> = Vec::new();
        let mut cur_expr = body;
        let root_ident = loop {
            match cur_expr {
                Expr::MethodCall(mc) => {
                    calls.push((&mc.method, mc.args.iter().collect()));
                    cur_expr = &mc.receiver;
                }
                Expr::Path(p) => {
                    let Some(root_ident) = p.path.get_ident() else {
                        return Err(syn::Error::new(
                            p.span(),
                            "`.fan` sub-chain must be rooted at the closure parameter",
                        ));
                    };
                    break root_ident;
                }
                other => {
                    return Err(syn::Error::new(
                        other.span(),
                        "`.fan` closure body must be a fluent method chain rooted at its parameter",
                    ));
                }
            }
        };
        if root_ident != param {
            return Err(syn::Error::new(
                root_ident.span(),
                format!("`.fan` sub-chain must start from its parameter `{param}`"),
            ));
        }
        calls.reverse();
        let mut cur = root;
        for call in &calls {
            let (method, args) = (call.0, &call.1);
            let anon = self.anon_name(method.span());
            cur = self.apply_call(cur, method, args, anon)?;
        }
        Ok(cur)
    }
}

/// Parse a `map_n` / `fan` repeat count: it must be an integer literal so the
/// unrolled DAG is known at expansion time.
fn parse_static_count(method: &Ident, arg: &Expr) -> syn::Result<usize> {
    if let Expr::Lit(lit) = arg
        && let syn::Lit::Int(int) = &lit.lit
    {
        return int.base10_parse::<usize>();
    }
    Err(syn::Error::new(
        arg.span(),
        format!("`.{method}(..)` repeat count must be an integer literal — the DAG must be static"),
    ))
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

/// The `merge_all` bad-argument error. Its fan-in must be spelled as a slice
/// literal of stream references so the macro can see the individual edges; a
/// runtime-built slice would make the DAG non-static, like a non-literal
/// `map_n` count.
fn merge_all_arg_error(arg: &Expr) -> syn::Error {
    syn::Error::new(
        arg.span(),
        "`.merge_all(..)` takes a slice literal of stream references — \
         `merge_all(&[&a, &b])` — so the merged edges are visible statically",
    )
}

impl Parse for NitroDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let wire_fn: ItemFn = input.parse()?;
        if !input.is_empty() {
            return Err(input.error("nitro! takes exactly one function definition"));
        }
        let sig = &wire_fn.sig;
        if !sig.generics.params.is_empty() || sig.generics.where_clause.is_some() {
            return Err(syn::Error::new(
                sig.generics.span(),
                "nitro functions cannot be generic",
            ));
        }
        if sig.asyncness.is_some() || sig.constness.is_some() || sig.unsafety.is_some() {
            return Err(syn::Error::new(
                sig.span(),
                "nitro functions must be plain `fn`s",
            ));
        }

        // The first parameter is the builder; its name roots source chains.
        // Any further parameters are input streams (`name: &Stream<T>`) —
        // the graph's edges in, roots for combinator chains.
        let mut params = sig.inputs.iter();
        let Some(syn::FnArg::Typed(builder_param)) = params.next() else {
            return Err(syn::Error::new(
                sig.inputs.span(),
                "nitro functions take the builder as their first parameter, e.g. \
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
                    "nitro functions take no self",
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
                "nitro functions must return `Stream<T>` (or a tuple of `Stream<T>`s)",
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
            let ix = walker.push_node(name.clone(), None, vec![], vec![], None);
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
                                 supported inside nitro! (compiled() inlines closures after \
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
            if walker.nodes[ix].is_input() {
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

        Ok(NitroDef {
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
pub fn nitro(input: TokenStream) -> TokenStream {
    let def = parse_macro_input!(input as NitroDef);
    let out = expand(&def);
    if std::env::var_os("NITRO_DEBUG").is_some() {
        eprintln!("=== nitro! {} ===\n{}", def.name, out);
    }
    out.into()
}

/// `#[op(build = <name>)]` — attribute on an `impl Op for X` block that also
/// emits the interpreted engine's `Builder::<name>` wiring method, so an op's
/// semantics and its interpreted entry point are single-sourced.
///
/// The generated method takes one `Handle` parameter per edge of the op's
/// `type In<'a>`, in order, then the op's `Cfg` (omitted when it is `()`), and
/// returns the output `Handle`. It covers every `In` shape the macro parses:
/// **sources** (`In = ()`), **single- and multi-input** ops, edges read with
/// their **tick flag** (`(&'a T, bool)` — `delay`, `merge`), any per-edge
/// **`passive` mask**, the op's **`start` / `stop` / `teardown` hooks**, and —
/// with `init_arg` — a **seeded accumulator** whose state and value slot come
/// from a call-site argument rather than `Default`. Node labels derive from
/// `type_name::<X>()` rather than a hand-written string.
///
/// `no_builder` opts out, for ops whose interpreted *signature* deliberately
/// differs from their shape: `bimap`/`trimap` take runtime active/passive
/// flags rather than a compile-time mask, and `with_time` seeds its value slot
/// from the input's current value so it never requires `Out: Default`.
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

/// Parsed `#[op(...)]` arguments:
///
/// - `build = <name>` — the fluent/graph method name (required, first);
/// - `fluent` — also emit `__wf_fluent_<name>!`, a `macro_rules!` that writes
///   this op's fluent extension-trait *method* (see [`expand_fluent`]); invoke
///   it in the trait's `impl` block. The op's `In` decides the receiver and so
///   the invocation form — `__wf_fluent_<name>!(T);` when edge 0 is a type
///   parameter, `__wf_fluent_<name>!();` for a concrete edge 0 or a source.
///   Needs the generated `Builder` method, so it is incompatible with
///   `no_builder`;
/// - `no_builder` — skip the interpreted `Builder` method, for the few ops
///   whose hand-written wiring has a deliberately different signature
///   (`bimap`/`trimap`, `with_time`);
/// - `passive = [i, ...]` — the listed edge positions are **passive** (read
///   but not activating; sample's data edge is `passive = [0]`). Becomes the
///   `__WF_OP_<NAME>_PASSIVE` bitmask const the emission folds into dispatch
///   conditions;
/// - `init_arg` — the **seeded-accumulator** shape (fold): the call site's
///   single plain argument is the initial `State`, cloned into both the
///   state and the value slot (so passive reads before the first tick see
///   the seed), and the op's `Cfg` is a literal closure passed by value. The
///   generated `Builder` method takes that seed as an `init` parameter ahead
///   of the config — `fold(src, init, f)`.
struct OpArgs {
    build: Ident,
    no_builder: bool,
    init_arg: bool,
    fluent: bool,
    passive: u32,
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
        let mut no_builder = false;
        let mut init_arg = false;
        let mut fluent = false;
        let mut passive: u32 = 0;
        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            let flag: Ident = input.parse()?;
            match flag.to_string().as_str() {
                "no_builder" => no_builder = true,
                "init_arg" => init_arg = true,
                "fluent" => fluent = true,
                "passive" => {
                    input.parse::<Token![=]>()?;
                    let content;
                    syn::bracketed!(content in input);
                    for lit in content.parse_terminated(syn::LitInt::parse, Token![,])? {
                        let pos: u32 = lit.base10_parse()?;
                        if pos >= 32 {
                            return Err(syn::Error::new(
                                lit.span(),
                                "passive edge positions must be < 32",
                            ));
                        }
                        passive |= 1 << pos;
                    }
                }
                other => {
                    return Err(syn::Error::new(
                        flag.span(),
                        format!(
                            "unknown #[op] flag `{other}`; expected `no_builder`, \
                             `init_arg`, `fluent`, or `passive = [..]`"
                        ),
                    ));
                }
            }
        }
        Ok(OpArgs {
            build,
            no_builder,
            init_arg,
            fluent,
            passive,
        })
    }
}

/// True for the unit type `()` — a `Cfg = ()` op takes no config argument.
fn is_unit_type(ty: &Type) -> bool {
    matches!(ty, Type::Tuple(t) if t.elems.is_empty())
}

fn is_bool(ty: &Type) -> bool {
    matches!(ty, Type::Path(p) if p.path.is_ident("bool"))
}

/// Strip lifetimes from a type (`&'a T` -> `&T`), recursively — forwarder
/// signatures use elided lifetimes where `Op::In<'a>` names one.
fn strip_lifetimes(ty: &Type) -> Type {
    let mut ty = ty.clone();
    strip_lifetimes_mut(&mut ty);
    ty
}

fn strip_lifetimes_mut(ty: &mut Type) {
    match ty {
        Type::Reference(r) => {
            r.lifetime = None;
            strip_lifetimes_mut(&mut r.elem);
        }
        Type::Tuple(t) => t.elems.iter_mut().for_each(strip_lifetimes_mut),
        Type::Path(p) => {
            for seg in p.path.segments.iter_mut() {
                if let syn::PathArguments::AngleBracketed(args) = &mut seg.arguments {
                    for arg in args.args.iter_mut() {
                        if let syn::GenericArgument::Type(t) = arg {
                            strip_lifetimes_mut(t);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// The identifiers appearing anywhere in a token stream — used to filter an
/// impl's generic params down to those a given type actually mentions.
fn idents_in(tokens: TokenStream2, out: &mut std::collections::HashSet<String>) {
    for tt in tokens {
        match tt {
            proc_macro2::TokenTree::Ident(i) => {
                out.insert(i.to_string());
            }
            proc_macro2::TokenTree::Group(g) => idents_in(g.stream(), out),
            _ => {}
        }
    }
}

fn idents_of(tokens: TokenStream2) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    idents_in(tokens, &mut set);
    set
}

/// The parsed shape of an `Op::In` tuple: its stream edges, and how to build
/// the op's `In` value from the **uniform pairs input** the `nitro!` emission
/// always passes — `((&v0, t0), (&v1, t1), ...)`, one `(value, tick)` pair
/// per edge. Element forms:
/// - `&T` — edge, value only;
/// - `(&T, bool)` — edge with its tick flag;
/// - bare `bool` — the tick flag of the *previous* edge (delay's flat form).
struct InShape {
    /// Per-edge value reference types, lifetimes stripped (`&T`).
    edge_ref_tys: Vec<Type>,
    /// Per-edge value types (`T`), for `'static` predicates.
    edge_val_tys: Vec<Type>,
    /// Per edge: does the op's `In` actually read that edge's tick flag? The
    /// interpreted builder only reads the engine's tick vector for edges
    /// where it does (every other edge's flag is a constant `false` the op
    /// never looks at).
    edge_uses_tick: Vec<bool>,
    /// Builds the op's `In` from the pairs param `__input`.
    in_expr: TokenStream2,
}

fn parse_in_shape(in_ty: &Type) -> syn::Result<InShape> {
    let Type::Tuple(tup) = in_ty else {
        return Err(syn::Error::new(
            in_ty.span(),
            "#[op]: `type In<'a>` must be a tuple (possibly empty for sources)",
        ));
    };
    let mut edge_ref_tys = Vec::new();
    let mut edge_val_tys = Vec::new();
    let mut edge_uses_tick: Vec<bool> = Vec::new();
    let mut elems: Vec<TokenStream2> = Vec::new();
    for elem in &tup.elems {
        match elem {
            Type::Reference(r) => {
                let ix = syn::Index::from(edge_ref_tys.len());
                edge_ref_tys.push(strip_lifetimes(elem));
                edge_val_tys.push(strip_lifetimes(&r.elem));
                edge_uses_tick.push(false);
                elems.push(quote! { __input.#ix.0 });
            }
            Type::Tuple(pair)
                if pair.elems.len() == 2
                    && matches!(pair.elems[0], Type::Reference(_))
                    && is_bool(&pair.elems[1]) =>
            {
                let ix = syn::Index::from(edge_ref_tys.len());
                let Type::Reference(r) = &pair.elems[0] else {
                    unreachable!("guarded by the match arm");
                };
                edge_ref_tys.push(strip_lifetimes(&pair.elems[0]));
                edge_val_tys.push(strip_lifetimes(&r.elem));
                edge_uses_tick.push(true);
                elems.push(quote! { (__input.#ix.0, __input.#ix.1) });
            }
            other if is_bool(other) => {
                if edge_ref_tys.is_empty() {
                    return Err(syn::Error::new(
                        other.span(),
                        "#[op]: a bare `bool` In element must follow the edge it is the tick of",
                    ));
                }
                let last = edge_ref_tys.len() - 1;
                edge_uses_tick[last] = true;
                let ix = syn::Index::from(last);
                elems.push(quote! { __input.#ix.1 });
            }
            other => {
                return Err(syn::Error::new(
                    other.span(),
                    "#[op]: In elements must be `&T` (edge), `(&T, bool)` (edge + tick), or \
                     `bool` (previous edge\'s tick)",
                ));
            }
        }
    }
    let in_expr = if tup.elems.is_empty() {
        quote! { () }
    } else {
        quote! { ( #(#elems,)* ) }
    };
    Ok(InShape {
        edge_ref_tys,
        edge_val_tys,
        edge_uses_tick,
        in_expr,
    })
}

fn expand_op(args: &OpArgs, imp: &ItemImpl) -> syn::Result<TokenStream2> {
    let self_ty = &*imp.self_ty;

    // Pull `In` / `Cfg` / `State` / `Out` off the impl\'s associated types,
    // the `ACTIVATION` expression off its associated const, and whether the
    // impl overrides `start`.
    let (mut in_ty, mut cfg_ty, mut state_ty, mut out_ty) = (None, None, None, None);
    let mut activation_expr: Option<Expr> = None;
    let mut overrides_start = false;
    let (mut overrides_stop, mut overrides_teardown) = (false, false);
    for it in &imp.items {
        match it {
            ImplItem::Type(t) => match t.ident.to_string().as_str() {
                "In" => in_ty = Some(t.ty.clone()),
                "Cfg" => cfg_ty = Some(t.ty.clone()),
                "State" => state_ty = Some(t.ty.clone()),
                "Out" => out_ty = Some(t.ty.clone()),
                _ => {}
            },
            ImplItem::Const(c) if c.ident == "ACTIVATION" => {
                activation_expr = Some(c.expr.clone());
            }
            ImplItem::Fn(f) => match f.sig.ident.to_string().as_str() {
                "start" => overrides_start = true,
                "stop" => overrides_stop = true,
                "teardown" => overrides_teardown = true,
                _ => {}
            },
            _ => {}
        }
    }
    let missing =
        |what: &str| syn::Error::new(imp.span(), format!("#[op]: impl has no `type {what}`"));
    let in_ty = in_ty.ok_or_else(|| missing("In"))?;
    let cfg_ty = cfg_ty.ok_or_else(|| missing("Cfg"))?;
    let state_ty = state_ty.ok_or_else(|| missing("State"))?;
    let out_ty = out_ty.ok_or_else(|| missing("Out"))?;
    let shape = parse_in_shape(&in_ty)?;
    let activation_expr = activation_expr
        .ok_or_else(|| syn::Error::new(imp.span(), "#[op]: impl has no `const ACTIVATION`"))?;

    let name = &args.build;

    // Emit the impl\'s generic *parameters* bare (`<A, B, F>`) and route every
    // bound — inline ones and the impl\'s own `where` — into a single `where`
    // clause, so a param is never bounded in two places
    // (`multiple_bound_locations`). Add the preds the slot machinery needs:
    // indexable edge values (`'static`) and a default-able output slot.
    let mut bare_params: Vec<TokenStream2> = Vec::new();
    let mut param_names: Vec<String> = Vec::new();
    let mut type_params: Vec<Ident> = Vec::new();
    let mut inline_bounds: Vec<(String, TokenStream2)> = Vec::new();
    let mut preds: Vec<TokenStream2> = Vec::new();
    for p in &imp.generics.params {
        match p {
            syn::GenericParam::Type(tp) => {
                let id = &tp.ident;
                bare_params.push(quote! { #id });
                param_names.push(id.to_string());
                type_params.push(id.clone());
                if !tp.bounds.is_empty() {
                    let bounds = &tp.bounds;
                    inline_bounds.push((id.to_string(), quote! { #id: #bounds }));
                    preds.push(quote! { #id: #bounds });
                }
            }
            other => bare_params.push(quote! { #other }),
        }
    }
    let mut where_preds_src: Vec<TokenStream2> = Vec::new();
    if let Some(w) = &imp.generics.where_clause {
        for pr in &w.predicates {
            where_preds_src.push(quote! { #pr });
            preds.push(quote! { #pr });
        }
    }
    for val_ty in &shape.edge_val_tys {
        preds.push(quote! { #val_ty: 'static });
    }
    // The forwarders always seed the value slot with `Default::default()`;
    // the generated `Builder` method does too *unless* `init_arg` seeds it
    // from the call site, so that predicate is added per consumer rather than
    // baked into the shared list (an `init_arg` accumulator need not be
    // `Default` — `fold` over a non-`Default` type stays legal fluently).
    let base_preds = preds.clone();
    preds.push(quote! { #out_ty: ::core::default::Default });
    let generics = if bare_params.is_empty() {
        quote! {}
    } else {
        quote! { <#(#bare_params),*> }
    };
    let where_tokens = quote! { where #(#preds),* };

    // ---- nitro! forwarders --------------------------------------------------
    //
    // Generic functions the `nitro!` emission calls by naming convention
    // (`__wf_op_<name>_cycle` ...), so the macro never names the op type —
    // rustc\'s inference resolves it from the argument types at the expansion
    // site. The emission is uniform: every edge arrives as a `(value, tick)`
    // pair; `in_expr` adapts the pairs to the op\'s declared `In` shape. The
    // `_owned` variants take the (literal-closure) `Cfg` by value as a direct
    // argument for the same inference deferral as `cycle_owned_cfg`.
    let cycle_fn = format_ident!("__wf_op_{}_cycle", name);
    let cycle_owned_fn = format_ident!("__wf_op_{}_cycle_owned", name);
    let start_fn = format_ident!("__wf_op_{}_start", name);
    let start_owned_fn = format_ident!("__wf_op_{}_start_owned", name);
    let stop_fn = format_ident!("__wf_op_{}_stop", name);
    let stop_owned_fn = format_ident!("__wf_op_{}_stop_owned", name);
    let teardown_fn = format_ident!("__wf_op_{}_teardown", name);
    let teardown_owned_fn = format_ident!("__wf_op_{}_teardown_owned", name);
    let seed_state_fn = format_ident!("__wf_op_{}_seed_state", name);
    let seed_value_fn = format_ident!("__wf_op_{}_seed_value", name);
    let upper = name.to_string().to_uppercase();
    let activation_const = format_ident!("__WF_OP_{}_ACTIVATION", upper);
    let passive_const = format_ident!("__WF_OP_{}_PASSIVE", upper);
    let passive_mask = args.passive;

    let edge_ref_tys = &shape.edge_ref_tys;
    let pairs_ty = if edge_ref_tys.is_empty() {
        quote! { () }
    } else {
        quote! { ( #((#edge_ref_tys, bool),)* ) }
    };
    let in_expr = &shape.in_expr;

    // Seed forwarders are *structural*: they return the State/Out type
    // written in the impl, generic only over the params those tokens mention
    // (plus the ignored cfg param `P`). Anything more would leave dangling
    // inference vars at the call site. The default is `Default::default()`;
    // ops needing config-derived seeds (fold) hand-write these forwarders.
    let seed_fn = |ret_ty: &Type, fn_name: &Ident| {
        let ret_idents = idents_of(quote! { #ret_ty });
        let kept: Vec<Ident> = param_names
            .iter()
            .filter(|p| ret_idents.contains(*p))
            .map(|p| format_ident!("{}", p))
            .collect();
        let kept_names: std::collections::HashSet<String> =
            kept.iter().map(|i| i.to_string()).collect();
        // A bound predicate is kept only if every impl generic it mentions
        // is kept — else it would drag in dangling params.
        let mut seed_preds: Vec<TokenStream2> = Vec::new();
        for (owner, bound) in &inline_bounds {
            if kept_names.contains(owner) {
                let mentioned = idents_of(bound.clone());
                if param_names
                    .iter()
                    .all(|p| !mentioned.contains(p) || kept_names.contains(p))
                {
                    seed_preds.push(bound.clone());
                }
            }
        }
        for pr in &where_preds_src {
            let mentioned = idents_of(pr.clone());
            if param_names.iter().any(|p| mentioned.contains(p))
                && param_names
                    .iter()
                    .all(|p| !mentioned.contains(p) || kept_names.contains(p))
            {
                seed_preds.push(pr.clone());
            }
        }
        seed_preds.push(quote! { #ret_ty: ::core::default::Default });
        quote! {
            #[doc(hidden)]
            #[inline(always)]
            #[allow(clippy::unused_unit)]
            pub fn #fn_name <#(#kept,)* __P> (_cfg: &__P) -> #ret_ty
            where #(#seed_preds),*
            {
                ::core::default::Default::default()
            }
        }
    };
    let (seed_state, seed_value) = if args.init_arg {
        // Seeded-accumulator shape: the call site's plain argument is the
        // initial `State`, cloned into the state *and* the value slot (so a
        // passive read before the first tick sees the seed — fold's legacy
        // parity). Generic only over the params the State/Out tokens
        // mention, like the Default-based seeds.
        let seed = |ret_ty: &Type, fn_name: &Ident| {
            let mentioned = {
                let mut set = idents_of(quote! { #state_ty });
                set.extend(idents_of(quote! { #ret_ty }));
                set
            };
            let kept: Vec<Ident> = param_names
                .iter()
                .filter(|p| mentioned.contains(*p))
                .map(|p| format_ident!("{}", p))
                .collect();
            let kept_names: std::collections::HashSet<String> =
                kept.iter().map(|i| i.to_string()).collect();
            let mut seed_preds: Vec<TokenStream2> = Vec::new();
            for (owner, bound) in &inline_bounds {
                if kept_names.contains(owner) {
                    let bound_mentions = idents_of(bound.clone());
                    if param_names
                        .iter()
                        .all(|p| !bound_mentions.contains(p) || kept_names.contains(p))
                    {
                        seed_preds.push(bound.clone());
                    }
                }
            }
            for pr in &where_preds_src {
                let pr_mentions = idents_of(pr.clone());
                if param_names.iter().any(|p| pr_mentions.contains(p))
                    && param_names
                        .iter()
                        .all(|p| !pr_mentions.contains(p) || kept_names.contains(p))
                {
                    seed_preds.push(pr.clone());
                }
            }
            seed_preds.push(quote! { #state_ty: ::core::clone::Clone });
            let kept_list = if kept.is_empty() {
                quote! {}
            } else {
                quote! { <#(#kept),*> }
            };
            quote! {
                #[doc(hidden)]
                #[inline(always)]
                pub fn #fn_name #kept_list (__init: &#state_ty) -> #ret_ty
                where #(#seed_preds),*
                {
                    ::core::clone::Clone::clone(__init)
                }
            }
        };
        (
            seed(&state_ty, &seed_state_fn),
            seed(&out_ty, &seed_value_fn),
        )
    } else {
        (
            seed_fn(&state_ty, &seed_state_fn),
            seed_fn(&out_ty, &seed_value_fn),
        )
    };

    // `_start_owned` is called when the op\'s config is a literal closure: the
    // closure cannot be re-created for the start call (its duplicate would
    // have un-inferable parameter types), so the convention is that
    // closure-config ops have no config-touching start hook. When the impl
    // does not override `start`, that is a plain no-op, fully generic so no
    // op params dangle. An op that overrides `start` *and* has a closure
    // config must hand-write its forwarders (this fn is then not emitted, so
    // the call site fails to resolve rather than silently skipping the hook).
    let start_owned = if overrides_start {
        quote! {}
    } else {
        quote! {
            #[doc(hidden)]
            #[inline(always)]
            pub fn #start_owned_fn <__Plain, __State> (
                _plain: &mut __Plain,
                _state: &mut __State,
                _ctx: &mut crate::op::Ctx<'_>,
            ) -> ::anyhow::Result<()> {
                ::core::result::Result::Ok(())
            }
        }
    };

    // `_start`, same principle as the seeds: when the impl does not override
    // `start` the forwarder is a fully-erased no-op — a real forwarding fn
    // would carry op generics that `Cfg`/`State` may not mention (filter's
    // `T`), leaving un-inferable type vars at the call site. When `start`
    // *is* overridden the real forwarder is emitted with the op's generics;
    // such ops (ticker, constant) anchor them through `Cfg`/`State`.
    let start = if overrides_start {
        quote! {
            #[doc(hidden)]
            #[inline(always)]
            pub fn #start_fn #generics (
                __cfg: &mut #cfg_ty,
                __state: &mut #state_ty,
                __ctx: &mut crate::op::Ctx<'_>,
            ) -> ::anyhow::Result<()>
            #where_tokens
            {
                <#self_ty as crate::op::Op>::start(__cfg, __state, __ctx)
            }
        }
    } else {
        quote! {
            #[doc(hidden)]
            #[inline(always)]
            pub fn #start_fn <__Cfg, __State> (
                _cfg: &mut __Cfg,
                _state: &mut __State,
                _ctx: &mut crate::op::Ctx<'_>,
            ) -> ::anyhow::Result<()> {
                ::core::result::Result::Ok(())
            }
        }
    };

    // `_cycle_owned`'s plain slot is unused `()` normally; under `init_arg`
    // it is the accumulator seed (typed `State`), which the cycle ignores —
    // the seeds consumed it at declaration time. `_cycle` (hoisted non-closure
    // config) is not emitted under `init_arg`: that shape would need a
    // `(init, cfg)` tuple whose seeding one signature cannot express — a
    // seeded op's config must be a literal closure (hand-write otherwise).
    let owned_plain_ty = if args.init_arg {
        quote! { #state_ty }
    } else {
        quote! { () }
    };
    let cycle_owned = quote! {
        #[doc(hidden)]
        #[inline(always)]
        pub fn #cycle_owned_fn #generics (
            mut __cfg: #cfg_ty,
            __plain: &mut #owned_plain_ty,
            __state: &mut #state_ty,
            __input: #pairs_ty,
            __ctx: &mut crate::op::Ctx<'_>,
        ) -> ::anyhow::Result<crate::op::Tick<#out_ty>>
        #where_tokens
        {
            <#self_ty as crate::op::Op>::cycle(&mut __cfg, __state, #in_expr, __ctx)
        }
    };
    let cycle = if args.init_arg {
        quote! {}
    } else {
        quote! {
            #[doc(hidden)]
            #[inline(always)]
            pub fn #cycle_fn #generics (
                __cfg: &mut #cfg_ty,
                __state: &mut #state_ty,
                __input: #pairs_ty,
                __ctx: &mut crate::op::Ctx<'_>,
            ) -> ::anyhow::Result<crate::op::Tick<#out_ty>>
            #where_tokens
            {
                <#self_ty as crate::op::Op>::cycle(__cfg, __state, #in_expr, __ctx)
            }
        }
    };

    // `_stop` / `_teardown` — the end-of-run lifecycle hooks. Emitted for
    // *every* op and always *real* (calling `<X as Op>::stop`/`teardown`,
    // whose trait default is a no-op that constant-folds away): the `nitro!`
    // tail can then call them for every node unconditionally without the macro
    // knowing which ops override a hook. Crucially they mirror
    // `cycle` / `cycle_owned` *exactly* — same `#pairs_ty` input parameter,
    // ignored by the hook — so rustc anchors the op's generics (and, in the
    // `_owned` variant, a literal-closure config's argument types) from the
    // call-site input the same way `cycle` does. A fully-erased no-op could
    // not: a bare closure literal handed to a signature that never relates it
    // to a typed input fails inference (E0282). The `_owned` variant threads
    // the closure through by value so a `finally` closure reaches its
    // `teardown`.
    let lifecycle = |plain_fn: &Ident, owned_fn: &Ident, hook: &Ident| {
        quote! {
            #[doc(hidden)]
            #[inline(always)]
            pub fn #plain_fn #generics (
                __cfg: &mut #cfg_ty,
                __state: &mut #state_ty,
                __input: #pairs_ty,
                __ctx: &mut crate::op::Ctx<'_>,
            ) -> ::anyhow::Result<()>
            #where_tokens
            {
                let _ = __input;
                <#self_ty as crate::op::Op>::#hook(__cfg, __state, __ctx)
            }

            #[doc(hidden)]
            #[inline(always)]
            pub fn #owned_fn #generics (
                mut __cfg: #cfg_ty,
                __plain: &mut #owned_plain_ty,
                __state: &mut #state_ty,
                __input: #pairs_ty,
                __ctx: &mut crate::op::Ctx<'_>,
            ) -> ::anyhow::Result<()>
            #where_tokens
            {
                let _ = (__plain, __input);
                <#self_ty as crate::op::Op>::#hook(&mut __cfg, __state, __ctx)
            }
        }
    };
    let stop = lifecycle(&stop_fn, &stop_owned_fn, &format_ident!("stop"));
    let teardown = lifecycle(&teardown_fn, &teardown_owned_fn, &format_ident!("teardown"));

    let forwarders = quote! {
        #[doc(hidden)]
        pub const #activation_const: crate::op::Activation = #activation_expr;
        /// Bit i set = edge i is passive (does not activate the node).
        #[doc(hidden)]
        pub const #passive_const: u32 = #passive_mask;

        #cycle

        #cycle_owned

        #start
        #start_owned
        #stop
        #teardown
        #seed_state
        #seed_value
    };

    // ---- interpreted Builder method, and the fluent method over it ---------
    let bshape = BuilderShape {
        self_ty,
        type_params: &type_params,
        shape: &shape,
        cfg_ty: &cfg_ty,
        state_ty: &state_ty,
        out_ty: &out_ty,
        generics: &generics,
        preds: &base_preds,
        overrides_start,
        overrides_stop,
        overrides_teardown,
    };
    if args.fluent && args.no_builder {
        return Err(syn::Error::new(
            args.build.span(),
            "#[op]: `fluent` needs the generated `Builder` method it forwards to — \
             remove `no_builder`, or write the fluent method by hand",
        ));
    }
    let builder = if args.no_builder {
        quote! {}
    } else {
        expand_builder(args, &bshape)?
    };
    let fluent = if args.fluent {
        expand_fluent(args, &bshape)?
    } else {
        quote! {}
    };

    Ok(quote! {
        #forwarders
        #builder
        #fluent
    })
}

/// What [`expand_builder`] needs from the `#[op]` impl to write the op's
/// interpreted `Builder` method.
struct BuilderShape<'a> {
    self_ty: &'a Type,
    /// The impl's type parameters, in declaration order (`[A, B, F]`).
    type_params: &'a [Ident],
    shape: &'a InShape,
    cfg_ty: &'a Type,
    state_ty: &'a Type,
    out_ty: &'a Type,
    /// The impl's generic parameters, bare (`<A, B, F>`).
    generics: &'a TokenStream2,
    /// The impl's own bounds, routed into one `where` list. Does **not**
    /// include the `Out: Default` the forwarders add — see `base_preds`.
    preds: &'a [TokenStream2],
    overrides_start: bool,
    overrides_stop: bool,
    overrides_teardown: bool,
}

/// Generate the op's interpreted `Builder` method — the wiring the engine
/// would otherwise carry by hand, once per op.
///
/// This covers **every** `In` shape [`parse_in_shape`] accepts: zero or more
/// edges, each read by value (`&T`) or with its tick flag (`(&T, bool)`, or a
/// trailing bare `bool`), any per-edge `passive` mask, the op's `start` /
/// `stop` / `teardown` hooks, and either a `Default`-seeded state or an
/// `init_arg` seed taken from the call site. The emitted body is exactly the
/// sequence the `#[op]` wiring seam on `Builder` exposes
/// (`next_node_index` → `slot`/`new_slot` → `push_node` → the `set_*`
/// attachers), so a hand-written builder and a generated one are the same
/// code; the remaining hand-written ones exist because their *signature*
/// differs from the op's shape (`bimap`/`trimap` take runtime active/passive
/// flags; `with_time` seeds its slot from the input rather than `Default`),
/// not because the shape is out of reach.
fn expand_builder(args: &OpArgs, b: &BuilderShape<'_>) -> syn::Result<TokenStream2> {
    let (self_ty, shape) = (b.self_ty, b.shape);
    let (cfg_ty, state_ty, out_ty) = (b.cfg_ty, b.state_ty, b.out_ty);
    let (generics, name) = (b.generics, &args.build);
    let n = shape.edge_ref_tys.len();
    if n > 26 {
        return Err(syn::Error::new(
            self_ty.span(),
            "#[op]: a generated Builder method supports at most 26 input edges",
        ));
    }
    if n < 32 && args.passive >> n != 0 {
        return Err(syn::Error::new(
            self_ty.span(),
            format!("#[op]: `passive` names an edge position beyond this op's {n} inputs"),
        ));
    }

    // One parameter per edge, in `In` order — `src` when there is only one
    // (reading `map(src, f)`), else `a`, `b`, `c`, … (reading `join(a, b, f)`).
    let edge_names: Vec<Ident> = (0..n)
        .map(|i| {
            if n == 1 {
                format_ident!("src")
            } else {
                format_ident!("{}", char::from(b'a' + i as u8))
            }
        })
        .collect();
    let slot_ids: Vec<Ident> = (0..n).map(|i| format_ident!("__slot{i}")).collect();
    let up_ids: Vec<Ident> = (0..n).map(|i| format_ident!("__up{i}")).collect();
    let borrow_ids: Vec<Ident> = (0..n).map(|i| format_ident!("__val{i}")).collect();
    let edge_params = edge_names
        .iter()
        .zip(&shape.edge_val_tys)
        .map(|(nm, ty)| quote! { #nm: crate::interp::Handle<#ty> });

    // Active vs passive edges: an active edge triggers the node when it ticks
    // (it goes into `push_node`'s upstream list); a passive one is read but
    // recorded separately, only so `build()` can raise this node's dispatch
    // layer above the value it reads.
    let (mut active, mut passive) = (Vec::new(), Vec::new());
    for (i, up) in up_ids.iter().enumerate() {
        if args.passive & (1 << i) == 0 {
            active.push(up);
        } else {
            passive.push(up);
        }
    }
    let passive_stmt = if passive.is_empty() {
        quote! {}
    } else {
        quote! { self.set_passive_ups(__idx, ::std::vec![#(#passive),*]); }
    };

    // Only edges whose `In` shape reads a tick flag cost a lookup in the
    // engine's tick vector; the rest are handed a constant `false` the op
    // never reads.
    let uses_ticks = shape.edge_uses_tick.iter().any(|t| *t);
    let ticked_let = if uses_ticks {
        quote! { let __ticked = self.ticked_flags(); }
    } else {
        quote! {}
    };
    let tick_lets = shape
        .edge_uses_tick
        .iter()
        .enumerate()
        .filter(|(_, used)| **used)
        .map(|(i, _)| {
            let (t, up) = (format_ident!("__tick{i}"), &up_ids[i]);
            quote! { let #t = __ticked.borrow()[#up]; }
        });
    let tick_exprs: Vec<TokenStream2> = shape
        .edge_uses_tick
        .iter()
        .enumerate()
        .map(|(i, used)| {
            if *used {
                let t = format_ident!("__tick{i}");
                quote! { #t }
            } else {
                quote! { false }
            }
        })
        .collect();

    // The call into the op, with every edge's slot borrowed for exactly the
    // duration of the call: the borrows end with the block, before the tick's
    // value is written back to the output slot.
    let in_expr = &shape.in_expr;
    let cycle_call = if n == 0 {
        quote! { <#self_ty as crate::op::Op>::cycle(__cfg, __state, (), &mut __ctx)? }
    } else {
        quote! {{
            #(let #borrow_ids = #slot_ids.borrow();)*
            let __input = ( #((&*#borrow_ids, #tick_exprs),)* );
            <#self_ty as crate::op::Op>::cycle(__cfg, __state, #in_expr, &mut __ctx)?
        }}
    };

    // `Cfg = ()` ops take no config argument; an `init_arg` op takes its
    // accumulator seed ahead of the config, matching `fold(src, init, f)`.
    let has_cfg = !is_unit_type(cfg_ty);
    let cfg_param = if has_cfg {
        quote! { cfg: #cfg_ty, }
    } else {
        quote! {}
    };
    let cfg_arg = if has_cfg {
        quote! { cfg }
    } else {
        quote! { () }
    };
    let (init_param, state_seed, out_seed, reset_init) = if args.init_arg {
        (
            quote! { init: #state_ty, },
            quote! { ::core::clone::Clone::clone(&init) },
            quote! { ::core::clone::Clone::clone(&init) },
            quote! { let __init_reset = init; },
        )
    } else {
        (
            quote! {},
            quote! { <#state_ty as ::core::default::Default>::default() },
            quote! { <#out_ty as ::core::default::Default>::default() },
            quote! {},
        )
    };
    let (state_reseed, out_reseed) = if args.init_arg {
        (
            quote! { ::core::clone::Clone::clone(&__init_reset) },
            quote! { ::core::clone::Clone::clone(&__init_reset) },
        )
    } else {
        (
            quote! { <#state_ty as ::core::default::Default>::default() },
            quote! { <#out_ty as ::core::default::Default>::default() },
        )
    };

    // Lifecycle hooks: emitted only where the impl overrides one, so an op
    // without them pays nothing (`push_node`'s defaults are no-ops).
    let hook = |cell: &Ident, method: &Ident| {
        quote! {
            ::std::boxed::Box::new(move |__k| {
                let (__cfg, __state) = &mut *#cell.borrow_mut();
                let mut __ctx = crate::op::Ctx::new(__k, __idx);
                <#self_ty as crate::op::Op>::#method(__cfg, __state, &mut __ctx)
            })
        }
    };
    let (start_clone, start_hook) = if b.overrides_start {
        let cell = format_ident!("__cs_start");
        let body = hook(&cell, &format_ident!("start"));
        (quote! { let #cell = __cs.clone(); }, body)
    } else {
        (
            quote! {},
            quote! { ::std::boxed::Box::new(|_| ::core::result::Result::Ok(())) },
        )
    };
    let (stop_clone, stop_stmt) = if b.overrides_stop {
        let cell = format_ident!("__cs_stop");
        let body = hook(&cell, &format_ident!("stop"));
        (
            quote! { let #cell = __cs.clone(); },
            quote! { self.set_stop(#body); },
        )
    } else {
        (quote! {}, quote! {})
    };
    let (teardown_clone, teardown_stmt) = if b.overrides_teardown {
        let cell = format_ident!("__cs_teardown");
        let body = hook(&cell, &format_ident!("teardown"));
        (
            quote! { let #cell = __cs.clone(); },
            quote! { self.set_teardown(#body); },
        )
    } else {
        (quote! {}, quote! {})
    };

    let mut preds: Vec<TokenStream2> = b.preds.to_vec();
    preds.push(quote! { #cfg_ty: 'static });
    preds.push(quote! { #state_ty: 'static });
    preds.push(quote! { #out_ty: 'static });
    if args.init_arg {
        preds.push(quote! { #state_ty: ::core::clone::Clone });
    } else {
        preds.push(quote! { #state_ty: ::core::default::Default });
        preds.push(quote! { #out_ty: ::core::default::Default });
    }

    let doc = format!(
        "Interpreted-engine wiring for [`{name}`]. Generated by `#[op]` from the op's \
         `In` shape — one `Handle` parameter per input edge, then its config."
    );
    Ok(quote! {
        impl crate::interp::Builder {
            #[doc = #doc]
            #[allow(clippy::too_many_arguments)]
            pub fn #name #generics (
                &mut self,
                #(#edge_params,)*
                #init_param
                #cfg_param
            ) -> crate::interp::Handle<#out_ty>
            where #(#preds),*
            {
                let __idx = self.next_node_index();
                #(let #up_ids = #edge_names.index();)*
                #(let #slot_ids = self.slot(#edge_names);)*
                let __out = self.new_slot::<#out_ty>(#out_seed);
                let __cs = ::std::rc::Rc::new(::core::cell::RefCell::new((
                    #cfg_arg,
                    #state_seed,
                )));
                #reset_init
                #ticked_let
                #start_clone
                #stop_clone
                #teardown_clone
                let __cs_cycle = __cs.clone();
                let __out_cycle = __out.clone();
                let (__cs_reset, __out_reset) = (__cs, __out);
                self.push_node(
                    ::std::vec![#(#active),*],
                    <#self_ty as crate::op::Op>::ACTIVATION,
                    crate::interp::short_type_name(::core::any::type_name::<#self_ty>()),
                    ::std::boxed::Box::new(move |__k| {
                        let (__cfg, __state) = &mut *__cs_cycle.borrow_mut();
                        let mut __ctx = crate::op::Ctx::new(__k, __idx);
                        #(#tick_lets)*
                        let __tick = #cycle_call;
                        match __tick {
                            crate::op::Tick::Value(__v) => {
                                *__out_cycle.borrow_mut() = __v;
                                ::core::result::Result::Ok(true)
                            }
                            crate::op::Tick::Silent(__v) => {
                                *__out_cycle.borrow_mut() = __v;
                                ::core::result::Result::Ok(false)
                            }
                            crate::op::Tick::Quiet => ::core::result::Result::Ok(false),
                        }
                    }),
                    #start_hook,
                );
                #stop_stmt
                #teardown_stmt
                #passive_stmt
                // A re-run restores the op's state and value slot to their
                // wiring-time values (see `ResetFn`).
                self.set_reset(::std::boxed::Box::new(move || {
                    __cs_reset.borrow_mut().1 = #state_reseed;
                    *__out_reset.borrow_mut() = #out_reseed;
                }));
                self.make_handle(__idx)
            }
        }
    })
}

/// Replace every occurrence of the identifier `from` with the tokens `to`,
/// recursively through groups — used to rewrite the op's first-edge type
/// parameter into the fluent macro's `$t` metavariable.
fn subst_ident(ts: TokenStream2, from: &Ident, to: &TokenStream2) -> TokenStream2 {
    ts.into_iter()
        .flat_map(|tt| match tt {
            proc_macro2::TokenTree::Ident(ref i) if i == from => {
                to.clone().into_iter().collect::<Vec<_>>()
            }
            proc_macro2::TokenTree::Group(g) => {
                vec![proc_macro2::TokenTree::Group(proc_macro2::Group::new(
                    g.delimiter(),
                    subst_ident(g.stream(), from, to),
                ))]
            }
            other => vec![other],
        })
        .collect()
}

/// Writes the op's **fluent method**, emitted as a `macro_rules!` that the
/// hand-written extension trait's `impl` block invokes.
///
/// The method itself is pure boilerplate over the generated `Builder` method:
/// the first input edge becomes `&self`, every later edge becomes a
/// `&Stream<_>` parameter whose handle is taken, and the op's config (plus an
/// `init_arg` seed) passes straight through — so the whole body is one
/// `self.wire(|b, h| b.<name>(h, ..))`.
///
/// Three receiver shapes are covered, and the op's own `In` decides which —
/// see [`FluentReceiver`]. The macro's invocation form is the tell: an op
/// whose first edge is a type parameter takes that type
/// (`__wf_fluent_map!(T);`), and the other two take nothing
/// (`__wf_fluent_rolling_mean!();`, `__wf_fluent_ticker!();`).
///
/// It is emitted as a `macro_rules!` rather than as an inherent method on
/// `Stream<T>` deliberately. An inherent impl would weld every op's method to
/// `Stream` itself, which is exactly the closed vocabulary the extension-trait
/// design exists to avoid; a macro is invoked from *whatever* trait impl the
/// op's author picks — `StreamOps` here, an adapter's own trait elsewhere.
/// (`#[op]` is in-crate-only today: its `Builder` method names
/// `crate::interp::Builder` and the attribute is not re-exported. The emitted
/// macro is `$crate`-clean and `#[macro_export]`ed regardless, so it keeps
/// working if that changes.)
///
/// The trait **declaration** stays hand-written: it is the documented public
/// surface, and because rustc checks the generated body against it, a
/// signature that drifts from the op's shape is a compile error rather than a
/// silent mismatch.
///
/// One shape stays hand-written by construction rather than oversight: an op
/// whose fluent signature orders its parameters differently from the generated
/// `(edges.., init, cfg)` — `delay_with_reset(delay, trigger)` — cannot be
/// expressed by a forwarder that follows the builder's order.
fn expand_fluent(args: &OpArgs, b: &BuilderShape<'_>) -> syn::Result<TokenStream2> {
    let (shape, name) = (b.shape, &args.build);
    let n = shape.edge_ref_tys.len();

    // Edge 0 is the receiver whenever the op has one, because the generated
    // body forwards through `Stream::wire`, which passes the receiver's handle
    // to the `Builder` method first.
    let receiver = if n == 0 {
        FluentReceiver::Source
    } else {
        let ty = &shape.edge_val_tys[0];
        let bare = match ty {
            Type::Path(p) => p
                .path
                .get_ident()
                .filter(|id| b.type_params.iter().any(|tp| tp == *id))
                .cloned(),
            _ => None,
        };
        match bare {
            Some(id) => FluentReceiver::Generic(id),
            // Concrete is *not* the fallback for everything that is not a bare
            // parameter: a type that merely mentions one — `Burst<T>` — has
            // something the invoking impl must supply, but no single ident to
            // bind it to. Generating `Concrete` for it would emit a macro
            // taking no `$t` whose body still says `T`, and the author would
            // meet an unresolved-name error at the invocation site with
            // nothing pointing back here. Reject it where the shape is known.
            None if mentions_any(ty, b.type_params) => {
                return Err(syn::Error::new_spanned(
                    ty,
                    "#[op(fluent)] cannot derive a receiver from this edge: it \
                     mentions one of the op's type parameters but is not itself \
                     a bare type parameter, so the generated macro has no single \
                     ident to bind. Make edge 0 either a bare parameter (`T` → \
                     `Stream<T>`) or fully concrete (`f64` → `Stream<f64>`), or \
                     drop `fluent` and write the method by hand.",
                ));
            }
            None => FluentReceiver::Concrete,
        }
    };
    let self_param = match &receiver {
        FluentReceiver::Generic(id) => Some(id.clone()),
        _ => None,
    };

    // Under a generic receiver, every mention of that parameter becomes the
    // macro's `$t`, so the invoking impl binds the receiver's element type.
    // The other two receivers name no type the impl has to supply, so their
    // macros take no argument and substitute nothing.
    let dollar_t = quote! { $t };
    let sub = |ts: TokenStream2| match &self_param {
        Some(p) => subst_ident(ts, p, &dollar_t),
        None => ts,
    };

    // The method's own generics are the impl's, less the receiver's.
    let method_generics: Vec<&Ident> = b
        .type_params
        .iter()
        .filter(|tp| Some(*tp) != self_param.as_ref())
        .collect();
    let generics = if method_generics.is_empty() {
        quote! {}
    } else {
        quote! { <#(#method_generics),*> }
    };

    // Edges 1.. become `&Stream<_>` parameters; their handles are taken
    // before the wiring closure so it can capture them by value.
    let edge_ids: Vec<Ident> = (1..n).map(|i| format_ident!("__e{i}")).collect();
    let edge_tys: Vec<TokenStream2> = shape
        .edge_val_tys
        .iter()
        .skip(1)
        .map(|ty| sub(quote! { #ty }))
        .collect();
    let edge_params: Vec<TokenStream2> = edge_ids
        .iter()
        .zip(&edge_tys)
        .map(|(id, ty)| quote! { #id: &$crate::fluent::Stream<#ty> })
        .collect();

    let (state_ty, cfg_ty, raw_out) = (b.state_ty, b.cfg_ty, b.out_ty);
    let (init_param, init_arg) = if args.init_arg {
        let ty = sub(quote! { #state_ty });
        (quote! { __init: #ty, }, quote! { __init, })
    } else {
        (quote! {}, quote! {})
    };
    let (cfg_param, cfg_arg) = if is_unit_type(b.cfg_ty) {
        (quote! {}, quote! {})
    } else {
        let ty = sub(quote! { #cfg_ty });
        (quote! { __cfg: #ty, }, quote! { __cfg, })
    };

    let out_ty = sub(quote! { #raw_out });
    // Mirror `expand_builder`'s predicate list exactly, including its
    // deliberate asymmetry: an `init_arg` accumulator is seeded from the call
    // site, so it is `Clone` but need *not* be `Default` (`fold` over a
    // non-`Default` type stays legal).
    let mut preds: Vec<TokenStream2> = b.preds.iter().map(|p| sub(p.clone())).collect();
    // The slot-machinery predicates are added only for types that are a bare
    // generic parameter of the generated method, or the receiver's own element
    // type (which the macro's `$t` names). `macro_rules!` resolves type
    // paths at the *invocation* site, so naming a concrete associated type
    // here (`WindowState<T>`, private to the op's module) would not resolve in
    // the extension trait's file — and does not need to: for a concrete type
    // rustc discharges `Default`/`'static` structurally, without a bound.
    let mut bound = |ty: &Type, bounds: TokenStream2| {
        let is_generic = matches!(ty, Type::Path(p)
            if p.path.get_ident().is_some_and(|id|
                Some(id) == self_param.as_ref() || method_generics.contains(&id)));
        if is_generic {
            preds.push(sub(quote! { #ty: #bounds }));
        }
    };
    bound(cfg_ty, quote! { 'static });
    bound(state_ty, quote! { 'static });
    bound(raw_out, quote! { 'static });
    if args.init_arg {
        bound(state_ty, quote! { ::core::clone::Clone });
    } else {
        bound(state_ty, quote! { ::core::default::Default });
        bound(raw_out, quote! { ::core::default::Default });
    }

    // A source has no upstream handle to thread, so it wires through
    // `GraphBuilder::source` rather than `Stream::wire`; everything else about
    // the method — generics, config, output — is identical.
    let body = if matches!(receiver, FluentReceiver::Source) {
        quote! { self.source(move |__b| __b.#name(#init_arg #cfg_arg)) }
    } else {
        quote! {
            #(let #edge_ids = #edge_ids.handle();)*
            self.wire(move |__b, __h| __b.#name(__h, #(#edge_ids,)* #init_arg #cfg_arg))
        }
    };

    let mac = format_ident!("__wf_fluent_{name}");
    let (matcher, invocation, doc_tail) = match &receiver {
        FluentReceiver::Generic(_) => (
            quote! { ($t:ty) },
            format!("{mac}!(T);"),
            "over the trait's element type `T`".to_string(),
        ),
        FluentReceiver::Concrete => {
            let ty = &shape.edge_val_tys[0];
            (
                quote! { () },
                format!("{mac}!();"),
                format!(
                    "over `Stream<{}>`, the concrete receiver its first edge fixes",
                    quote! { #ty }
                ),
            )
        }
        FluentReceiver::Source => (
            quote! { () },
            format!("{mac}!();"),
            "on a `GraphBuilder` extension trait (it is a source — no receiver \
             stream)"
                .to_string(),
        ),
    };
    let doc = format!(
        "Fluent wiring for [`{name}`], generated by `#[op(fluent)]`. Invoke it \
         inside an extension-trait `impl` — `{invocation}` — to define the \
         `{name}` method {doc_tail}."
    );
    let signal = expand_signal(SignalShape {
        name,
        receiver: &receiver,
        matcher: &matcher,
        generics: &generics,
        edge_ids: &edge_ids,
        edge_tys: &edge_tys,
        init_param: &init_param,
        init_arg: &init_arg,
        cfg_param: &cfg_param,
        cfg_arg: &cfg_arg,
        out_ty: &out_ty,
        preds: &preds,
    });
    Ok(quote! {
        #[doc = #doc]
        #[macro_export]
        macro_rules! #mac {
            #matcher => {
                fn #name #generics (
                    &self,
                    #(#edge_params,)*
                    #init_param
                    #cfg_param
                ) -> $crate::fluent::Stream<#out_ty>
                where #(#preds),*
                {
                    #body
                }
            };
        }
        #signal
    })
}

/// The pieces [`expand_fluent`] has already derived that [`expand_signal`]
/// re-uses verbatim. Everything here is computed once, against the op's shape;
/// the `Signal` twin differs only in the types it wraps and the body it emits.
struct SignalShape<'a> {
    name: &'a Ident,
    receiver: &'a FluentReceiver,
    matcher: &'a TokenStream2,
    generics: &'a TokenStream2,
    edge_ids: &'a [Ident],
    edge_tys: &'a [TokenStream2],
    init_param: &'a TokenStream2,
    init_arg: &'a TokenStream2,
    cfg_param: &'a TokenStream2,
    cfg_arg: &'a TokenStream2,
    out_ty: &'a TokenStream2,
    preds: &'a [TokenStream2],
}

/// Writes the op's [`Signal`](../wingfoil_next/signal/index.html) method —
/// the implicit-graph facade's twin of the fluent one — as
/// `__wf_signal_<name>!`.
///
/// `Signal<T>` is a newtype over `Stream<T>` carrying the graph and a slot for
/// its `Runner`, so every one of its combinators is the same forward: unwrap
/// the receiver and each edge, wire the op, re-wrap. That is mechanical enough
/// that the facade drifted — it was missing 15 of `StreamOps`' 41 methods, one
/// per op nobody remembered to hand-forward — so the macro writes it.
///
/// The unwrap/re-wrap goes through `Signal::as_stream` and `Signal::wrap`,
/// the `pub(crate)` pair the facade documents as its seam — the same
/// arrangement as the `Builder` wiring seam [`expand_builder`] targets. This
/// generator therefore names no private field of a type it does not own, and
/// `signal.rs` stays free to change its representation.
///
/// The body wires through `Stream::wire` and the op's generated `Builder`
/// method — the same target the fluent method forwards to — rather than
/// calling the fluent method itself. That keeps the `Signal` method's bounds
/// exactly the op's: a hand-written trait declaration is free to be *stricter*
/// than the op needs (`StreamOps::accumulate` asks for `T: Default` where the
/// op, whose `Out` is a `Vec<T>`, does not), and routing through it would
/// import that stricter bound into a signature the generator did not write.
///
/// Unlike the fluent method these are **inherent** methods, so there is no
/// hand-written trait declaration for rustc to check the body against. The
/// whole method is generated, docs included: they point at the op's `Builder`
/// method, whose docs are the op witness type's, so the semantics are stated
/// once and the facade cannot describe them differently from the engine.
///
/// A source gets no method — it enters the facade as a free function that
/// makes the graph (`signal::ticker`), which is a different shape and stays
/// hand-written.
///
/// The expansion targets a `pub(crate)` seam, so it only compiles inside
/// `wingfoil-next`. That is not a limitation to design around — these are
/// inherent methods on a type this crate owns, so no other crate could invoke
/// the macro usefully anyway. It is why the macro is `__`-prefixed and
/// undocumented in the public API despite `#[macro_export]`, which it needs
/// only to be visible at the crate root where `signal.rs` invokes it.
fn expand_signal(s: SignalShape<'_>) -> TokenStream2 {
    if matches!(s.receiver, FluentReceiver::Source) {
        return quote! {};
    }
    let (name, generics, matcher) = (s.name, s.generics, s.matcher);
    let (edge_ids, init_arg, cfg_arg) = (s.edge_ids, s.init_arg, s.cfg_arg);
    let (init_param, cfg_param, out_ty, preds) = (s.init_param, s.cfg_param, s.out_ty, s.preds);
    let edge_params = edge_ids
        .iter()
        .zip(s.edge_tys)
        .map(|(id, ty)| quote! { #id: &$crate::signal::Signal<#ty> });

    let mac = format_ident!("__wf_signal_{name}");
    let method_doc = format!(
        "The [`Signal`](crate::signal::Signal) form of [`{name}`]\
         (crate::interp::Builder::{name}) — see there for what it computes and \
         when it ticks. Generated by `#[op(fluent)]`."
    );
    let doc = format!(
        "`Signal` wiring for [`{name}`], generated by `#[op(fluent)]`. Invoke \
         it inside an `impl Signal<..>` block — the expansion goes through \
         `Signal`'s `pub(crate)` `as_stream` / `wrap` seam, so it compiles \
         only inside `wingfoil-next`."
    );
    quote! {
        #[doc = #doc]
        #[macro_export]
        macro_rules! #mac {
            #matcher => {
                #[doc = #method_doc]
                pub fn #name #generics (
                    &self,
                    #(#edge_params,)*
                    #init_param
                    #cfg_param
                ) -> $crate::signal::Signal<#out_ty>
                where #(#preds),*
                {
                    #(let #edge_ids = #edge_ids.as_stream().handle();)*
                    self.wrap(self.as_stream().wire(
                        move |__b, __h| __b.#name(__h, #(#edge_ids,)* #init_arg #cfg_arg)
                    ))
                }
            };
        }
    }
}

/// Whether `ty` mentions any of `params` at any depth.
///
/// Token-level on purpose. The question [`expand_fluent`] asks is only
/// "does the invoking impl have to supply something for this type", and an
/// occurrence of the parameter's ident anywhere answers yes — no type
/// resolution needed. A false positive (a path segment that merely shares a
/// parameter's name) costs an error telling the author to hand-write the
/// method, which is what the generator did for every non-bare shape before it
/// learned the concrete receiver; a false *negative* is what this exists to
/// prevent, and tokens cannot produce one.
fn mentions_any(ty: &Type, params: &[Ident]) -> bool {
    fn walk(ts: TokenStream2, params: &[Ident]) -> bool {
        ts.into_iter().any(|tt| match tt {
            TokenTree::Ident(id) => params.contains(&id),
            TokenTree::Group(g) => walk(g.stream(), params),
            _ => false,
        })
    }
    walk(ty.to_token_stream(), params)
}

/// Which of the three receiver shapes an op's [generated fluent
/// method](expand_fluent) takes — decided by the op's own `In`, not by the
/// author. A shape that is none of the three is [rejected with a
/// message](expand_fluent), never silently forced into one.
enum FluentReceiver {
    /// Edge 0 is one of the impl's type parameters, so the invoking trait impl
    /// substitutes its element type for it: the macro takes that type as `$t`
    /// (`StreamOps`, and any adapter trait generic in its payload).
    Generic(Ident),
    /// Edge 0 is a fixed type (`&f64` — the statistics ops, the augurs
    /// adapter), so the receiver is `Stream<f64>` and there is nothing for the
    /// invoking impl to supply.
    Concrete,
    /// No edges at all: the receiver is the `GraphBuilder`, and the body wires
    /// through `GraphBuilder::source` (`SourceOps::ticker`, `constant`).
    Source,
}

fn expand(def: &NitroDef) -> TokenStream2 {
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
            // In-crate `#[op]` forwarders (`__wf_op_*`), so catalog ops reach
            // the generic fallback without an import at the call site. User
            // ops' forwarders arrive through `use super::*`.
            #[allow(unused_imports)]
            use ::wingfoil_next::ops::*;
            #wire_fn
            #standalone
            #nested
        }
    }
}

fn expand_interpreted(def: &NitroDef) -> TokenStream2 {
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

/// cfg + state + value-slot declarations for one node — **uniform for every
/// op**: the plain call arguments become the `__cfg` local (one value, or a
/// tuple in call order), and the state and value slots are seeded through the
/// op's `_seed_state` / `_seed_value` forwarders. The macro writes no types
/// anywhere; inference resolves them through the forwarder calls (fold's
/// hand-written seeds clone the accumulator init into both, so a passive
/// read before the first tick sees `init` — legacy parity).
fn node_decl(node: &NodeDef) -> TokenStream2 {
    // Inputs have no storage here — their value/tick are read from the outer
    // graph in the `nested` prologue.
    if node.is_input() {
        return quote! {};
    }
    let name = &node.name;
    let cfg = format_ident!("__cfg_{}", name);
    let state = format_ident!("__state_{}", name);
    let value = format_ident!("__v_{}", name);
    let seed_state = node.forwarder("seed_state");
    let seed_value = node.forwarder("seed_value");
    let cfg_decl = if node.has_cfg() {
        let e = node.cfg_expr();
        quote! { let mut #cfg = #e; }
    } else {
        quote! {}
    };
    let cfg_ref = if node.has_cfg() {
        quote! { &#cfg }
    } else {
        quote! { &() }
    };
    quote! {
        #cfg_decl
        let mut #state = #seed_state(#cfg_ref);
        let mut #value = #seed_value(#cfg_ref);
    }
}

/// Passthrough statements interleaved with node declarations in source
/// order: a config expression must see exactly the passthrough bindings
/// that preceded its wiring statement in `wire()` (shadowing included).
fn interleaved_setup(def: &NitroDef) -> Vec<TokenStream2> {
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

/// The start call for one node: `_start` for plain configs, `_start_owned`
/// for literal-closure configs (which the start convention cannot see — the
/// forwarder is a no-op unless the op overrides `start`, in which case it
/// must hand-write its forwarders). `Op::start` has a no-op default, so
/// calling unconditionally costs nothing after monomorphization.
fn node_start(target: Target, idx: usize, node: &NodeDef) -> TokenStream2 {
    let cfg = format_ident!("__cfg_{}", node.name);
    let state = format_ident!("__state_{}", node.name);
    let ctx = ctx_expr(target, idx);
    let cfg_arg = if node.has_cfg() {
        quote! { &mut #cfg }
    } else {
        quote! { &mut () }
    };
    let fwd = if node.closure.is_some() {
        node.forwarder("start_owned")
    } else {
        node.forwarder("start")
    };
    quote! {
        {
            let mut __ctx = #ctx;
            #fwd(#cfg_arg, &mut #state, &mut __ctx)?;
        }
    }
}

/// One end-of-run lifecycle call (`stop` or `teardown`) for one node, run at
/// the loop tail. Unlike `start`, cleanup is **error-safe**: a hook error is
/// captured into `__first_err` (first-error-wins) rather than propagated, so
/// every node's stop and teardown still run after an earlier abort — mirroring
/// the interpreted `Runner`. Closure-config nodes route through the `_owned`
/// forwarder, threading the literal closure through by value (as `cycle_owned`
/// does) so a `finally` closure is available to its `teardown`.
fn node_lifecycle(def: &NitroDef, target: Target, idx: usize, hook: &str) -> TokenStream2 {
    let node = &def.nodes[idx];
    let cfg = format_ident!("__cfg_{}", node.name);
    let state = format_ident!("__state_{}", node.name);
    let ctx = ctx_expr(target, idx);
    let input = lifecycle_input(def, node);
    let cfg_arg = if node.has_cfg() {
        quote! { &mut #cfg }
    } else {
        quote! { &mut () }
    };
    let call = match &node.closure {
        Some(f) => {
            let fwd = node.forwarder(&format!("{hook}_owned"));
            quote! { #fwd(#f, #cfg_arg, &mut #state, #input, &mut __ctx) }
        }
        None => {
            let fwd = node.forwarder(hook);
            quote! { #fwd(#cfg_arg, &mut #state, #input, &mut __ctx) }
        }
    };
    quote! {
        {
            let mut __ctx = #ctx;
            if let ::core::result::Result::Err(__e) = #call {
                __first_err.get_or_insert(__e);
            }
        }
    }
}

/// The dispatch line for one node: condition, forwarder cycle call, tick
/// flag. The condition composes, per edge, a passive-mask guard
/// (`__WF_OP_<M>_PASSIVE`, sample's data edge), plus the activation-const
/// guards for busy-poll (`always`) and callback (`schedules`/`threaded`)
/// activation — all consts, folded away after monomorphization for the ops
/// that do not use them.
fn node_dispatch(def: &NitroDef, target: Target, i: usize) -> TokenStream2 {
    let node = &def.nodes[i];
    let name = &node.name;
    let cfg = format_ident!("__cfg_{}", name);
    let state = format_ident!("__state_{}", name);
    let value = format_ident!("__v_{}", name);
    let ticked = format_ident!("__t_{}", name);
    let idx = i;
    let act = node.op_const("ACTIVATION");
    let passive = node.op_const("PASSIVE");

    // A variadic op is all-active by construction, so it needs no per-edge
    // passive-mask guard — and must not have one: the mask is a `u32`, so a
    // fan-in wider than 32 would shift past its width.
    let mut cond_parts: Vec<TokenStream2> = node
        .refs
        .iter()
        .enumerate()
        .map(|(pos, &u)| {
            let t = format_ident!("__t_{}", def.nodes[u].name);
            if node.variadic {
                return quote! { #t };
            }
            let p = pos as u32;
            quote! { (((#passive >> #p) & 1) == 0 && #t) }
        })
        .collect();
    cond_parts.push(quote! { #act.always });
    cond_parts.push(quote! { (#act.callback_activated() && __dirty[#idx]) });
    let cond = quote! { #(#cond_parts)||* };

    let input = cycle_input(def, node);
    let cfg_arg = if node.has_cfg() {
        quote! { &mut #cfg }
    } else {
        quote! { &mut () }
    };
    // A literal closure goes by value as a DIRECT argument so rustc defers
    // its signature inference until `input` has resolved the op's value
    // types (behind `&mut` that deferral does not apply); it is a zero-cost
    // ZST rebuilt each cycle. Non-closure configs were hoisted once into
    // `__cfg_<name>` by `node_decl`.
    let cycle_call = match &node.closure {
        Some(f) => {
            let fwd = node.forwarder("cycle_owned");
            quote! { #fwd(#f, #cfg_arg, &mut #state, #input, &mut __ctx) }
        }
        None => {
            let fwd = node.forwarder("cycle");
            quote! { #fwd(#cfg_arg, &mut #state, #input, &mut __ctx) }
        }
    };
    let ctx = ctx_expr(target, idx);
    // `?` propagates an op error out of the enclosing `compiled()` fn or the
    // `nested()` composite closure — both return `anyhow::Result`.
    let call = quote! {
        {
            let mut __ctx = #ctx;
            match #cycle_call? {
                ::wingfoil_next::op::Tick::Value(__val) => {
                    #value = __val;
                    true
                }
                ::wingfoil_next::op::Tick::Silent(__val) => {
                    #value = __val;
                    false
                }
                ::wingfoil_next::op::Tick::Quiet => false,
            }
        }
    };
    // Every node gets a named tick flag: downstream cycle inputs are uniform
    // `(value, tick)` pairs, so any consumer may read it. `let _` silences
    // the terminal-node case.
    quote! { let #ticked = (#cond) && #call; let _ = #ticked; }
}

fn expand_compiled(def: &NitroDef) -> TokenStream2 {
    let n = def.nodes.len();
    let setup = interleaved_setup(def);
    let mut starts = Vec::new();
    let mut body = Vec::new();
    let mut stops = Vec::new();
    let mut teardowns = Vec::new();
    for (i, node) in def.nodes.iter().enumerate() {
        starts.push(node_start(Target::Compiled, i, node));
        body.push(node_dispatch(def, Target::Compiled, i));
        stops.push(node_lifecycle(def, Target::Compiled, i, "stop"));
        teardowns.push(node_lifecycle(def, Target::Compiled, i, "teardown"));
    }

    let out_types: Vec<&Type> = def.outs.iter().map(|(_, t)| t).collect();
    let out_values: Vec<TokenStream2> =
        def.outs.iter().map(|(o, _)| output_value_expr(o)).collect();

    quote! {
        /// The same graph, fully monomorphized: node state in locals, tick
        /// propagation as bools, every `Op::cycle` (closures included)
        /// visible to the compiler. Derived from the same tokens as
        /// `interpreted`, so the two cannot drift.
        // A side-effect-only sink (`finally`, `print`, `for_each`) is a node
        // with no downstream and no output slot read, so its `__v_`/`__state_`
        // locals are write-only — allowed, as in `nested`.
        #[allow(unused_mut, unused_variables, unused_assignments)]
        pub fn compiled(
            run_mode: ::wingfoil_next::RunMode,
            run_for: ::wingfoil_next::RunFor,
        ) -> ::wingfoil_next::anyhow::Result<( #(#out_types,)* )> {
            let mut __k = ::wingfoil_next::Kernel::new(run_mode, run_for);
            #(#setup)*
            // Cleanup (stop/teardown) must run even when `start` or a cycle
            // aborts, so the run phase is an immediately-invoked closure whose
            // `?` is *captured* into `__first_err` rather than early-returning
            // out of `compiled`. After it, every node's `stop` then `teardown`
            // runs in node order — each get-or-inserting its error — and the
            // outputs are returned only if nothing errored. This mirrors the
            // interpreted `Runner`: cleanup always runs, first error wins.
            let mut __first_err: ::core::option::Option<::wingfoil_next::anyhow::Error> =
                ::core::option::Option::None;
            let __run = (|| -> ::wingfoil_next::anyhow::Result<()> {
                #(#starts)*
                let mut __dirty = [false; #n];
                while __k.begin_cycle(&mut __dirty) {
                    #(#body)*
                    __k.end_cycle(&mut __dirty);
                }
                ::core::result::Result::Ok(())
            })();
            if let ::core::result::Result::Err(__e) = __run {
                __first_err = ::core::option::Option::Some(__e);
            }
            #(#stops)*
            #(#teardowns)*
            match __first_err {
                ::core::option::Option::Some(__e) => ::core::result::Result::Err(__e),
                ::core::option::Option::None => {
                    ::core::result::Result::Ok(( #(#out_values,)* ))
                }
            }
        }
    }
}

/// The whole graph as a single node ("compiled island") mounted in an
/// interpreted graph. One closure owns all inner state; inner dispatch is
/// the same monomorphized straight-line code `compiled` emits, with inner
/// schedules demultiplexed through a private `TimeQueue` — only the
/// earliest is forwarded to the outer kernel.
fn expand_nested(def: &NitroDef) -> TokenStream2 {
    let n = def.nodes.len();
    let (out_name, out_ty) = &def.outs[0];
    let out_t = format_ident!("__t_{}", out_name);
    let out_v = output_value_expr(out_name);
    // Whether the island can be activated by kernel callbacks: the OR of
    // every inner op's `ACTIVATION` const — a handful of const bools,
    // evaluated once at wiring time.
    let act_consts: Vec<Ident> = def
        .nodes
        .iter()
        .filter(|node| !node.is_input())
        .map(|node| node.op_const("ACTIVATION"))
        .collect();
    let callback_activated = quote! {
        false #(|| #act_consts.callback_activated())*
    };
    let setup = interleaved_setup(def);
    let mut starts = Vec::new();
    let mut body = Vec::new();
    let mut stops = Vec::new();
    let mut teardowns = Vec::new();
    for (i, node) in def.nodes.iter().enumerate() {
        if node.is_input() {
            continue;
        }
        starts.push(node_start(Target::Nested, i, node));
        body.push(node_dispatch(def, Target::Nested, i));
        stops.push(node_lifecycle(def, Target::Nested, i, "stop"));
        teardowns.push(node_lifecycle(def, Target::Nested, i, "teardown"));
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
    // Only inputs consumed through a *non-passive* edge activate the
    // composite (sample's data edge must not). Per input, OR the passive-mask
    // guards of every consuming edge — consts, evaluated once at wiring.
    let input_active_exprs: Vec<TokenStream2> = def
        .inputs
        .iter()
        .enumerate()
        .map(|(input_ix, _)| {
            let mut parts: Vec<TokenStream2> = Vec::new();
            for consumer in def.nodes.iter().filter(|node| !node.is_input()) {
                let passive = consumer.op_const("PASSIVE");
                for (pos, &u) in consumer.refs.iter().enumerate() {
                    if u == input_ix {
                        let p = pos as u32;
                        parts.push(quote! { (((#passive >> #p) & 1) == 0) });
                    }
                }
            }
            quote! { false #(|| #parts)* }
        })
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
        #[allow(unused_mut, unused_variables, unused_assignments)]
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
            let mut __q = ::wingfoil_next::TimeQueue::<usize>::new();
            let mut __dirty = [false; #n];
            let mut __active: ::std::vec::Vec<usize> = ::std::vec::Vec::new();
            let mut __passive: ::std::vec::Vec<usize> = ::std::vec::Vec::new();
            #(
                if #input_active_exprs {
                    __active.push(#ix_ids);
                } else {
                    __passive.push(#ix_ids);
                }
            )*
            __g.__composite(__active, __passive, #callback_activated, move |__ctx, __phase| {
                let __now = __ctx.time();
                let __start_time = __ctx.start_time();
                match __phase {
                    ::wingfoil_next::op::CompositePhase::Start => {
                        #(#starts)*
                        if let Some(__t) = __q.next_time() {
                            __ctx.schedule(__t);
                        }
                        return ::core::result::Result::Ok(::wingfoil_next::op::Tick::Quiet);
                    }
                    // End-of-run cleanup, driven by the outer engine calling
                    // the composite node's stop/teardown. Error-safe: every
                    // inner op's hook runs, first error wins — the same
                    // contract `compiled` and the interpreted `Runner` honour.
                    ::wingfoil_next::op::CompositePhase::Stop => {
                        let mut __first_err:
                            ::core::option::Option<::wingfoil_next::anyhow::Error> =
                            ::core::option::Option::None;
                        #(#stops)*
                        return match __first_err {
                            ::core::option::Option::Some(__e) => {
                                ::core::result::Result::Err(__e)
                            }
                            ::core::option::Option::None => {
                                ::core::result::Result::Ok(::wingfoil_next::op::Tick::Quiet)
                            }
                        };
                    }
                    ::wingfoil_next::op::CompositePhase::Teardown => {
                        let mut __first_err:
                            ::core::option::Option<::wingfoil_next::anyhow::Error> =
                            ::core::option::Option::None;
                        #(#teardowns)*
                        return match __first_err {
                            ::core::option::Option::Some(__e) => {
                                ::core::result::Result::Err(__e)
                            }
                            ::core::option::Option::None => {
                                ::core::result::Result::Ok(::wingfoil_next::op::Tick::Quiet)
                            }
                        };
                    }
                    ::wingfoil_next::op::CompositePhase::Cycle => {}
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

/// The value expression for an output node (`o` is a resolved node name):
/// its `__v_<name>` slot. Every op — unit-valued sources included — keeps a
/// slot, seeded through its `_seed_value` forwarder.
fn output_value_expr(o: &Ident) -> TokenStream2 {
    let v = format_ident!("__v_{}", o);
    quote! { #v }
}

/// A `&`-expression for reading an upstream's current value. An input
/// upstream's local is a borrow guard on the outer graph's slot, so it is
/// re-borrowed rather than referenced.
fn upstream_value_expr(def: &NitroDef, ix: usize) -> TokenStream2 {
    let up = &def.nodes[ix];
    let ident = format_ident!("__v_{}", up.name);
    if up.is_input() {
        quote! { &*#ident }
    } else {
        quote! { &#ident }
    }
}

/// The uniform cycle input: one `(value, tick)` pair per edge, in edge order
/// (`()` for sources). The op's `_cycle` forwarder adapts the pairs to its
/// declared `In` shape — value-only, tick-flagged, or pairs-verbatim — so
/// the macro carries no per-op input-shape knowledge at all.
fn cycle_input(def: &NitroDef, node: &NodeDef) -> TokenStream2 {
    if node.refs.is_empty() {
        return quote! { () };
    }
    let pairs = node.refs.iter().map(|&u| {
        let v = upstream_value_expr(def, u);
        let t = format_ident!("__t_{}", def.nodes[u].name);
        quote! { (#v, #t) }
    });
    // A variadic op takes the same pairs as a *slice*, which is what lets one
    // forwarder signature serve a fan-in of any width (a tuple impl would cap
    // at whatever arity the crate spelled out).
    if node.variadic {
        return quote! { &[ #(#pairs,)* ] };
    }
    quote! { ( #(#pairs,)* ) }
}

/// The input passed to a node's `stop`/`teardown` forwarder. It has the same
/// `#pairs_ty` shape as [`cycle_input`] — the forwarders take it purely so
/// rustc anchors the op's generics (and a literal-closure config's argument
/// types) exactly as `cycle` does — but the hook ignores it, so the tick flags
/// are the literal `false` (the per-cycle `__t_<name>` locals are scoped to the
/// dispatch loop / cycle phase and are out of scope at the cleanup tail) and an
/// input edge is read straight from its captured slot rather than the
/// cycle-phase borrow guard.
fn lifecycle_input(def: &NitroDef, node: &NodeDef) -> TokenStream2 {
    if node.refs.is_empty() {
        return quote! { () };
    }
    let pairs = node.refs.iter().map(|&u| {
        let up = &def.nodes[u];
        let v = if up.is_input() {
            let slot = format_ident!("__slot_{}", up.name);
            quote! { &*#slot.borrow() }
        } else {
            let ident = format_ident!("__v_{}", up.name);
            quote! { &#ident }
        };
        quote! { (#v, false) }
    });
    if node.variadic {
        return quote! { &[ #(#pairs,)* ] };
    }
    quote! { ( #(#pairs,)* ) }
}

// ---------------------------------------------------------------------------
// latency_stages! — the shared latency-record generator
// ---------------------------------------------------------------------------
//
// Moved here from `wingfoil-derive` so that the shared latency data layer
// (`runtime::latency`) has no dependency on the legacy tree. The legacy
// `wingfoil` crate re-exports this macro, so `wingfoil::latency_stages!` is
// unchanged for legacy callers.

struct LatencyStagesInput {
    visibility: Visibility,
    name: Ident,
    stages: Vec<Ident>,
    type_name_override: Option<LitStr>,
}

impl Parse for LatencyStagesInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let attrs = input.call(Attribute::parse_outer)?;
        let mut type_name_override: Option<LitStr> = None;
        for attr in &attrs {
            if attr.path().is_ident("type_name") {
                if type_name_override.is_some() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "duplicate #[type_name(...)] attribute",
                    ));
                }
                let lit: LitStr = attr.parse_args().map_err(|_| {
                    syn::Error::new_spanned(
                        attr,
                        "expected #[type_name(\"...\")] with a single string literal",
                    )
                })?;
                type_name_override = Some(lit);
            } else {
                return Err(syn::Error::new_spanned(
                    attr,
                    "unrecognized attribute on latency_stages!; only #[type_name(\"...\")] is supported",
                ));
            }
        }

        let visibility: Visibility = input.parse()?;
        let name: Ident = input.parse()?;
        let content;
        braced!(content in input);
        let list = Punctuated::<Ident, Token![,]>::parse_terminated(&content)?;
        let stages: Vec<Ident> = list.into_iter().collect();
        if stages.is_empty() {
            return Err(syn::Error::new(
                name.span(),
                "latency_stages! requires at least one stage",
            ));
        }
        Ok(LatencyStagesInput {
            visibility,
            name,
            stages,
            type_name_override,
        })
    }
}

/// Convert `PascalCase` to `snake_case`. Used to derive the per-struct stage module name.
fn pascal_to_snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Declare a fixed-size, named-field latency record suitable for embedding in
/// a `#[repr(C)]` payload. Each field is a `u64` nanosecond timestamp.
///
/// The macro generates:
/// - A `#[repr(C)]` struct with one `pub <field>: u64` per stage.
/// - An `impl wingfoil::Latency` for the struct (gives slice access by index).
/// - A nested module with one zero-sized marker per stage, each implementing
///   `wingfoil::Stage<Self>` with the stage's compile-time index. Use those
///   markers as the type parameter to `.stamp::<S>()`.
///
/// # Example
///
/// ```rust,ignore
/// use wingfoil::*;
///
/// latency_stages! {
///     pub TradeLatency {
///         ingest,
///         decode,
///         strategy,
///         publish,
///     }
/// }
///
/// // Markers live in a snake_case sub-module named after the struct:
/// use trade_latency::strategy;
/// let stamped = upstream.stamp::<strategy>();
/// ```
#[proc_macro]
pub fn latency_stages(item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as LatencyStagesInput);
    let LatencyStagesInput {
        visibility,
        name,
        stages,
        type_name_override,
    } = input;

    let n = stages.len();
    let module_name = Ident::new(&pascal_to_snake(&name.to_string()), Span::call_site());
    let stage_strs: Vec<String> = stages.iter().map(|i| i.to_string()).collect();
    let stage_indices: Vec<usize> = (0..n).collect();
    let field_names = &stages;
    let marker_names = &stages;
    let zero_copy_send_body = match type_name_override {
        Some(lit) => quote! {
            unsafe fn type_name() -> &'static str { #lit }
        },
        None => quote! {},
    };

    let expanded = quote! {
        #[repr(C)]
        #[derive(
            ::std::clone::Clone, ::std::marker::Copy,
            ::std::fmt::Debug, ::std::default::Default,
            ::std::cmp::PartialEq, ::std::cmp::Eq,
            ::std::hash::Hash,
            ::serde::Serialize, ::serde::Deserialize,
        )]
        #visibility struct #name {
            #( pub #field_names: u64, )*
        }

        impl Latency for #name {
            const N: usize = #n;
            fn stage_names() -> &'static [&'static str] {
                &[ #( #stage_strs ),* ]
            }
            #[inline]
            fn stamps(&self) -> &[u64] {
                // SAFETY: `#[repr(C)]` struct of N consecutive u64 fields has
                // the same memory layout as `[u64; N]`.
                unsafe {
                    ::std::slice::from_raw_parts(
                        self as *const Self as *const u64,
                        <Self as Latency>::N,
                    )
                }
            }
            #[inline]
            fn stamp_mut(&mut self, idx: usize) -> &mut u64 {
                assert!(idx < <Self as Latency>::N, "stage index out of bounds");
                // SAFETY: see `stamps`; idx is bounds-checked above.
                unsafe { &mut *((self as *mut Self as *mut u64).add(idx)) }
            }
        }

        // SAFETY: `#[repr(C)]` packed `u64` fields are self-contained and
        // have a uniform memory representation, satisfying `ZeroCopySend`'s
        // invariants. Only emitted when the `iceoryx2` feature is on
        // in the consuming crate.
        #[cfg(feature = "iceoryx2")]
        unsafe impl ::iceoryx2::prelude::ZeroCopySend for #name {
            #zero_copy_send_body
        }

        #[allow(non_snake_case, non_camel_case_types)]
        #visibility mod #module_name {
            use super::*;
            #(
                /// Compile-time marker for a single latency stage.
                pub struct #marker_names;
                impl Stage<super::#name> for #marker_names {
                    const NAME: &'static str = #stage_strs;
                    const INDEX: usize = #stage_indices;
                }
            )*
        }
    };

    expanded.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    fn params(names: &[&str]) -> Vec<Ident> {
        names
            .iter()
            .map(|n| Ident::new(n, Span::call_site()))
            .collect()
    }

    /// The predicate that separates [`FluentReceiver::Concrete`] from the
    /// shapes `#[op(fluent)]` must reject.
    ///
    /// `Burst<T>` is the case this exists for: it is not a bare parameter, so
    /// it is not `Generic`, but it is not concrete either — treating it as
    /// `Concrete` would emit a macro taking no `$t` whose body still says `T`.
    #[test]
    fn mentions_any_sees_parameters_at_every_depth() {
        let t = params(&["T"]);
        for ty in [
            parse_quote!(T),
            parse_quote!(Burst<T>),
            parse_quote!(Vec<Option<T>>),
            parse_quote!((T, u64)),
            parse_quote!(&[T]),
        ] {
            let ty: Type = ty;
            assert!(mentions_any(&ty, &t), "{} mentions T", ty.to_token_stream());
        }
    }

    /// The other side: a genuinely concrete edge — the statistics ops' `f64`,
    /// and a generic type whose arguments are all concrete — stays `Concrete`,
    /// so the fixed-receiver shape keeps generating.
    #[test]
    fn mentions_any_ignores_types_with_no_parameter() {
        let t = params(&["T"]);
        for ty in [
            parse_quote!(f64),
            parse_quote!(Burst<u64>),
            parse_quote!(Vec<NanoTime>),
            parse_quote!((u64, bool)),
        ] {
            let ty: Type = ty;
            assert!(
                !mentions_any(&ty, &t),
                "{} mentions no parameter",
                ty.to_token_stream()
            );
        }
    }

    /// An op generic in more than one parameter must trip on any of them, not
    /// just the first.
    #[test]
    fn mentions_any_checks_every_parameter() {
        let ab = params(&["A", "B"]);
        let ty: Type = parse_quote!(Burst<B>);
        assert!(mentions_any(&ty, &ab));
    }
}

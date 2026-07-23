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
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    Expr, Ident, ImplItem, ItemFn, ItemImpl, Pat, ReturnType, Stmt, Token, Type, parse_macro_input,
    parse_quote,
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
                // Left-fold the branch tails through binary merges, matching
                // the fluent layer's N-ary "first supplied that ticked wins".
                let merge = Ident::new("merge", method.span());
                let mut merged = tails[0];
                for (j, &tail) in tails.iter().enumerate().skip(1) {
                    let node_name = if j + 1 == tails.len() {
                        name.clone()
                    } else {
                        self.anon_name(method.span())
                    };
                    merged = self.push_node(
                        node_name,
                        Some(merge.clone()),
                        vec![merged, tail],
                        vec![],
                        None,
                    );
                }
                merged
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

/// Parsed `#[op(...)]` arguments:
///
/// - `build = <name>` — the fluent/graph method name (required, first);
/// - `no_builder` — skip the interpreted `Builder` method (ops with
///   hand-written builder wiring: sources, multi-input, seeded shapes);
/// - `passive = [i, ...]` — the listed edge positions are **passive** (read
///   but not activating; sample's data edge is `passive = [0]`). Becomes the
///   `__WF_OP_<NAME>_PASSIVE` bitmask const the emission folds into dispatch
///   conditions;
/// - `init_arg` — the **seeded-accumulator** shape (fold): the call site's
///   single plain argument is the initial `State`, cloned into both the
///   state and the value slot (so passive reads before the first tick see
///   the seed), and the op's `Cfg` is a literal closure passed by value.
///   Implies `no_builder` (the interpreted side needs a seeded slot, which
///   `register_op1` does not express).
struct OpArgs {
    build: Ident,
    no_builder: bool,
    init_arg: bool,
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
        let mut passive: u32 = 0;
        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            let flag: Ident = input.parse()?;
            match flag.to_string().as_str() {
                "no_builder" => no_builder = true,
                "init_arg" => init_arg = true,
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
                             `init_arg`, or `passive = [..]`"
                        ),
                    ));
                }
            }
        }
        if init_arg && !no_builder {
            return Err(syn::Error::new(
                build.span(),
                "`init_arg` requires `no_builder`: the interpreted Builder method for a \
                 seeded op must seed its value slot, which the generated `register_op1` \
                 wrapper does not express — write it by hand",
            ));
        }
        Ok(OpArgs {
            build,
            no_builder,
            init_arg,
            passive,
        })
    }
}

/// The `T` of a single-element reference tuple type `(&'a T,)`, or an error if
/// the type is not that shape (which is how `#[op]` scopes its *Builder
/// method* generation; the `graph!` forwarders support any [`InShape`]).
fn single_ref_tuple_elem(ty: &Type) -> syn::Result<Type> {
    if let Type::Tuple(tup) = ty
        && tup.elems.len() == 1
        && let Type::Reference(r) = &tup.elems[0]
    {
        return Ok((*r.elem).clone());
    }
    Err(syn::Error::new(
        ty.span(),
        "#[op] generates a Builder method only for single-input ops (`type In<'a>` = `(&'a T,)`); \
         add `no_builder` to keep a hand-written Builder method and still get graph! forwarders",
    ))
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
/// the op's `In` value from the **uniform pairs input** the `graph!` emission
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
    let mut elems: Vec<TokenStream2> = Vec::new();
    for elem in &tup.elems {
        match elem {
            Type::Reference(r) => {
                let ix = syn::Index::from(edge_ref_tys.len());
                edge_ref_tys.push(strip_lifetimes(elem));
                edge_val_tys.push(strip_lifetimes(&r.elem));
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
                elems.push(quote! { (__input.#ix.0, __input.#ix.1) });
            }
            other if is_bool(other) => {
                if edge_ref_tys.is_empty() {
                    return Err(syn::Error::new(
                        other.span(),
                        "#[op]: a bare `bool` In element must follow the edge it is the tick of",
                    ));
                }
                let ix = syn::Index::from(edge_ref_tys.len() - 1);
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
            ImplItem::Fn(f) if f.sig.ident == "start" => {
                overrides_start = true;
            }
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
    let mut inline_bounds: Vec<(String, TokenStream2)> = Vec::new();
    let mut preds: Vec<TokenStream2> = Vec::new();
    for p in &imp.generics.params {
        match p {
            syn::GenericParam::Type(tp) => {
                let id = &tp.ident;
                bare_params.push(quote! { #id });
                param_names.push(id.to_string());
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
    preds.push(quote! { #out_ty: ::core::default::Default });
    let generics = if bare_params.is_empty() {
        quote! {}
    } else {
        quote! { <#(#bare_params),*> }
    };
    let where_tokens = quote! { where #(#preds),* };

    // ---- graph! forwarders --------------------------------------------------
    //
    // Generic functions the `graph!` emission calls by naming convention
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
        // passive read before the first tick sees the seed — fold's classic
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
        #seed_state
        #seed_value
    };

    // ---- interpreted Builder method (single-input ops only) ----------------
    let builder = if args.no_builder {
        quote! {}
    } else {
        let input_ty = single_ref_tuple_elem(&in_ty)?;
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
        let doc = format!("Interpreted-engine wiring for [`{name}`]. Generated by `#[op]`.");
        quote! {
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
        }
    };

    Ok(quote! {
        #forwarders
        #builder
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

/// cfg + state + value-slot declarations for one node — **uniform for every
/// op**: the plain call arguments become the `__cfg` local (one value, or a
/// tuple in call order), and the state and value slots are seeded through the
/// op's `_seed_state` / `_seed_value` forwarders. The macro writes no types
/// anywhere; inference resolves them through the forwarder calls (fold's
/// hand-written seeds clone the accumulator init into both, so a passive
/// read before the first tick sees `init` — classic parity).
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

/// The dispatch line for one node: condition, forwarder cycle call, tick
/// flag. The condition composes, per edge, a passive-mask guard
/// (`__WF_OP_<M>_PASSIVE`, sample's data edge), plus the activation-const
/// guards for busy-poll (`always`) and callback (`schedules`/`threaded`)
/// activation — all consts, folded away after monomorphization for the ops
/// that do not use them.
fn node_dispatch(def: &GraphDef, target: Target, i: usize) -> TokenStream2 {
    let node = &def.nodes[i];
    let name = &node.name;
    let cfg = format_ident!("__cfg_{}", name);
    let state = format_ident!("__state_{}", name);
    let value = format_ident!("__v_{}", name);
    let ticked = format_ident!("__t_{}", name);
    let idx = i;
    let act = node.op_const("ACTIVATION");
    let passive = node.op_const("PASSIVE");

    let mut cond_parts: Vec<TokenStream2> = node
        .refs
        .iter()
        .enumerate()
        .map(|(pos, &u)| {
            let t = format_ident!("__t_{}", def.nodes[u].name);
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

fn expand_compiled(def: &GraphDef) -> TokenStream2 {
    let n = def.nodes.len();
    let setup = interleaved_setup(def);
    let mut starts = Vec::new();
    let mut body = Vec::new();
    for (i, node) in def.nodes.iter().enumerate() {
        starts.push(node_start(Target::Compiled, i, node));
        body.push(node_dispatch(def, Target::Compiled, i));
    }

    let out_types: Vec<&Type> = def.outs.iter().map(|(_, t)| t).collect();
    let out_values: Vec<TokenStream2> =
        def.outs.iter().map(|(o, _)| output_value_expr(o)).collect();

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
    for (i, node) in def.nodes.iter().enumerate() {
        if node.is_input() {
            continue;
        }
        starts.push(node_start(Target::Nested, i, node));
        body.push(node_dispatch(def, Target::Nested, i));
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
            let mut __active: ::std::vec::Vec<usize> = ::std::vec::Vec::new();
            #(
                if #input_active_exprs {
                    __active.push(#ix_ids);
                }
            )*
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
fn upstream_value_expr(def: &GraphDef, ix: usize) -> TokenStream2 {
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
fn cycle_input(def: &GraphDef, node: &NodeDef) -> TokenStream2 {
    if node.refs.is_empty() {
        return quote! { () };
    }
    let pairs = node.refs.iter().map(|&u| {
        let v = upstream_value_expr(def, u);
        let t = format_ident!("__t_{}", def.nodes[u].name);
        quote! { (#v, #t) }
    });
    quote! { ( #(#pairs,)* ) }
}

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
//! `.join(&other, f)`, `.delay(duration)`; sugar `.count()` and
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
//! loops/conditionals — the DAG must be static); `external` sources are not
//! expressible (use the fluent API directly for those graphs).

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{Expr, Ident, ItemFn, Pat, ReturnType, Stmt, Type, parse_macro_input, parse_quote};

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
}

impl OpKind {
    /// Can this op be activated by a kernel callback (needs a dirty check)?
    fn callback_activated(self) -> bool {
        matches!(self, Self::Ticker | Self::Constant | Self::Delay)
    }

    /// Does this op have a `start` hook to run before the first cycle?
    fn has_start(self) -> bool {
        matches!(self, Self::Ticker | Self::Constant)
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
    /// Sample is the one op with a passive edge: its data source is read by
    /// value slot only; the trigger is its sole active edge.
    fn active_ups(&self) -> Vec<usize> {
        match self.op {
            OpKind::Sample => vec![self.refs[1]],
            _ => self.refs.clone(),
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

/// True if any identifier in `tokens` names the builder or a known stream —
/// used to reject graph wiring hidden inside arbitrary (passthrough) code,
/// which `wire()` would execute but `compiled()` could not see.
fn mentions_graph_ident(
    tokens: TokenStream2,
    builder: &Ident,
    streams: &std::collections::HashMap<String, usize>,
) -> bool {
    use proc_macro2::TokenTree;
    for tt in tokens {
        match tt {
            TokenTree::Ident(i) => {
                if i == *builder || streams.contains_key(&i.to_string()) {
                    return true;
                }
            }
            TokenTree::Group(gp) => {
                if mentions_graph_ident(gp.stream(), builder, streams) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
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
        Ident::new(&format!("anon{}", self.anon), span)
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
                other => {
                    return Err(syn::Error::new(
                        method.span(),
                        format!(
                            "unknown combinator `.{other}(..)`; expected one of: map, filter, \
                             fold, sample, merge, join, delay, count, accumulate"
                        ),
                    ));
                }
            };
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
            if mentions_graph_ident(tokens, &builder, &walker.index_of) {
                return Err(syn::Error::new(
                    stmt.span(),
                    "graph wiring must be straight-line `let name = <chain>;` statements — \
                     the builder and stream names cannot appear inside other code (compiled() \
                     could not see nodes built there)",
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
        }
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

/// cfg + state + value slot declarations for one node.
fn node_decl(node: &NodeDef) -> TokenStream2 {
    let name = &node.name;
    let cfg = format_ident!("__cfg_{}", name);
    let state = format_ident!("__state_{}", name);
    let value = format_ident!("__v_{}", name);
    let exprs = &node.exprs;
    match node.op {
        // Inputs have no storage here — their value/tick are read from the
        // outer graph in the `nested` prologue.
        OpKind::Input => quote! {},
        OpKind::Ticker => {
            let period = &exprs[0];
            quote! {
                let mut #cfg = ::wingfoil_next::wingfoil::NanoTime::from(#period);
                let mut #state: Option<::wingfoil_next::wingfoil::NanoTime> = None;
            }
        }
        OpKind::Constant => {
            let value_expr = &exprs[0];
            quote! {
                let mut #cfg = #value_expr;
                let mut #value = Default::default();
            }
        }
        // Closure-carrying ops keep no cfg local: the closure is inlined
        // at the cycle call (via `cycle_owned_cfg`) so direct-argument
        // deferral drives its signature inference.
        OpKind::Map | OpKind::Join => quote! {
            let mut #value = Default::default();
        },
        OpKind::Fold => {
            let init = &exprs[0];
            quote! {
                let mut #state = #init;
                let mut #value = Default::default();
            }
        }
        OpKind::Filter | OpKind::Sample | OpKind::Merge => quote! {
            let mut #value = Default::default();
        },
        OpKind::Delay => {
            let dur = &exprs[0];
            quote! {
                let mut #cfg = ::wingfoil_next::wingfoil::NanoTime::from(#dur);
                let mut #state = ::wingfoil_next::ops::DelayState::default();
                let mut #value = Default::default();
            }
        }
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
            #op_path::start(&mut #cfg, #state_arg, &mut __ctx);
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
    if node.op.callback_activated() {
        cond_parts.push(quote! { __dirty[#idx] });
    }
    let cond = quote! { #(#cond_parts)||* };

    let op_path = op_path(node.op);
    let input = cycle_input(def, node);
    let cfg_arg = cycle_cfg_arg(node, &cfg);
    let state_arg = cycle_state_arg(node, &state);
    let on_value = if node.op == OpKind::Ticker {
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
    let cycle_call = match node.op {
        // Closure configs go by value as a DIRECT argument so rustc
        // defers their signature inference until the input argument has
        // resolved the op's value types (a closure behind `&mut` loses
        // that deferral and fails E0282).
        OpKind::Map | OpKind::Join | OpKind::Fold => {
            let f = if node.op == OpKind::Fold {
                &node.exprs[1]
            } else {
                &node.exprs[0]
            };
            let ty = op_type(node.op);
            quote! {
                ::wingfoil_next::op::cycle_owned_cfg::<#ty>(#f, #state_arg, #input, &mut __ctx)
            }
        }
        _ => quote! { #op_path::cycle(#cfg_arg, #state_arg, #input, &mut __ctx) },
    };
    let ctx = ctx_expr(target, idx);
    let call = quote! {
        {
            let mut __ctx = #ctx;
            match #cycle_call {
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
        if node.op.has_start() {
            starts.push(node_start(Target::Compiled, i, node));
        }
        body.push(node_dispatch(def, Target::Compiled, i, flags[i]));
    }

    let out_types: Vec<&Type> = def.outs.iter().map(|(_, t)| t).collect();
    let out_values: Vec<TokenStream2> = def
        .outs
        .iter()
        .map(|(o, _)| {
            let v = format_ident!("__v_{}", o);
            quote! { #v }
        })
        .collect();

    quote! {
        /// The same graph, fully monomorphized: node state in locals, tick
        /// propagation as bools, every `Op::cycle` (closures included)
        /// visible to the compiler. Derived from the same tokens as
        /// `interpreted`, so the two cannot drift.
        pub fn compiled(
            run_mode: ::wingfoil_next::wingfoil::RunMode,
            run_for: ::wingfoil_next::wingfoil::RunFor,
        ) -> ( #(#out_types,)* ) {
            let mut __k = ::wingfoil_next::wingfoil::codegen::Kernel::new(run_mode, run_for);
            #(#setup)*
            #(#starts)*
            let mut __dirty = [false; #n];
            while __k.begin_cycle(&mut __dirty) {
                #(#body)*
                __k.end_cycle(&mut __dirty);
            }
            ( #(#out_values,)* )
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
    let out_v = format_ident!("__v_{}", out_name);
    let callback_activated = def.nodes.iter().any(|node| node.op.callback_activated());
    let flags = tick_flags_needed(def);
    let setup = interleaved_setup(def);
    let mut starts = Vec::new();
    let mut body = Vec::new();
    for (i, node) in def.nodes.iter().enumerate() {
        if node.op == OpKind::Input {
            continue;
        }
        if node.op.has_start() {
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
                    return ::wingfoil_next::op::Tick::Quiet;
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
                if #out_t {
                    ::wingfoil_next::op::Tick::Value(::core::clone::Clone::clone(&#out_v))
                } else {
                    ::wingfoil_next::op::Tick::Quiet
                }
            })
        }
    }
}

/// The concrete (inference-holed) op type.
fn op_type(op: OpKind) -> TokenStream2 {
    match op {
        OpKind::Input => unreachable!("inputs are not ops"),
        OpKind::Ticker => quote! { ::wingfoil_next::ops::Ticker },
        OpKind::Constant => quote! { ::wingfoil_next::ops::Const<_> },
        OpKind::Map => quote! { ::wingfoil_next::ops::Map<_, _, _> },
        OpKind::Filter => quote! { ::wingfoil_next::ops::Filter<_> },
        OpKind::Fold => quote! { ::wingfoil_next::ops::Fold<_, _, _> },
        OpKind::Sample => quote! { ::wingfoil_next::ops::Sample<_> },
        OpKind::Merge => quote! { ::wingfoil_next::ops::Merge2<_> },
        OpKind::Join => quote! { ::wingfoil_next::ops::Join<_, _, _, _> },
        OpKind::Delay => quote! { ::wingfoil_next::ops::Delay<_> },
    }
}

/// The op type wrapped as `<Type as Op>` so trait items resolve without
/// `Op` being in scope at the expansion site.
fn op_path(op: OpKind) -> TokenStream2 {
    let ty = op_type(op);
    quote! { <#ty as ::wingfoil_next::op::Op> }
}

fn cycle_cfg_arg(node: &NodeDef, cfg: &Ident) -> TokenStream2 {
    match node.op {
        OpKind::Filter | OpKind::Sample | OpKind::Merge => quote! { &mut () },
        _ => quote! { &mut #cfg },
    }
}

fn cycle_state_arg(node: &NodeDef, state: &Ident) -> TokenStream2 {
    match node.op {
        OpKind::Ticker | OpKind::Fold | OpKind::Delay => quote! { &mut #state },
        _ => quote! { &mut () },
    }
}

fn start_state_arg(node: &NodeDef, state: &Ident) -> TokenStream2 {
    match node.op {
        OpKind::Ticker => quote! { &mut #state },
        _ => quote! { &mut () },
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
    match node.op {
        OpKind::Input => unreachable!("inputs are not dispatched"),
        OpKind::Ticker | OpKind::Constant => quote! { () },
        OpKind::Map | OpKind::Fold | OpKind::Sample => {
            let src = v(node.refs[0]);
            quote! { (#src,) }
        }
        OpKind::Filter => {
            let src = v(node.refs[0]);
            let cond = v(node.refs[1]);
            quote! { (#src, #cond) }
        }
        OpKind::Merge => {
            let (a, b) = (v(node.refs[0]), v(node.refs[1]));
            let (ta, tb) = (t(node.refs[0]), t(node.refs[1]));
            quote! { ((#a, #ta), (#b, #tb)) }
        }
        OpKind::Join => {
            let (a, b) = (v(node.refs[0]), v(node.refs[1]));
            quote! { (#a, #b) }
        }
        OpKind::Delay => {
            let src = v(node.refs[0]);
            let ts = t(node.refs[0]);
            quote! { (#src, #ts) }
        }
    }
}

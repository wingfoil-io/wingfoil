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
//!   compiler.
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
//! The function must take exactly one parameter (the builder, any name) and
//! return `Stream<T>` — or a tuple `(Stream<A>, Stream<B>, ...)` with a
//! matching tuple of bound names as the tail expression. Supported chain
//! methods mirror the fluent API: sources `.ticker(period)` /
//! `.constant(value)` on the builder; combinators `.map(f)`,
//! `.filter(&cond)`, `.fold(init, f)`, `.sample(&trigger)`, `.merge(&other)`,
//! `.join(&other, f)`, `.delay(duration)`; sugar `.count()` and
//! `.accumulate()`.
//!
//! Limitations (v1): the body must be straight-line `let` statements plus the
//! tail expression (no loops/conditionals — the DAG must be static); graphs
//! live at module scope so closures can reference items but not runtime
//! locals; `external` sources are not expressible (use the fluent API
//! directly for those graphs).

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{Expr, Ident, ItemFn, Pat, ReturnType, Stmt, Type, parse_macro_input, parse_quote};

#[derive(Clone, Copy, PartialEq)]
enum OpKind {
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
    /// Output bindings (from the tail expression) and their value types
    /// (from the `Stream<T>` return type), in order.
    outs: Vec<(Ident, Type)>,
    nodes: Vec<NodeDef>,
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

    /// A `&name` argument referencing another stream.
    fn stream_ref_arg(&self, arg: &Expr) -> syn::Result<usize> {
        if let Expr::Reference(r) = arg
            && let Expr::Path(p) = &*r.expr
            && let Some(ident) = p.path.get_ident()
        {
            return self.lookup(ident);
        }
        Err(syn::Error::new(
            arg.span(),
            "expected `&name` referencing a stream bound by an earlier `let`",
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

        // The single parameter is the builder; its name roots source chains.
        let mut params = sig.inputs.iter();
        let (Some(syn::FnArg::Typed(builder_param)), None) = (params.next(), params.next()) else {
            return Err(syn::Error::new(
                sig.inputs.span(),
                "graph functions take exactly one parameter: the builder, e.g. \
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

        // Walk the body: `let` chains, then a tail expression naming the
        // returned stream binding(s).
        let mut walker = ChainWalker::new();
        let mut tail: Option<&Expr> = None;
        let n_stmts = wire_fn.block.stmts.len();
        for (i, stmt) in wire_fn.block.stmts.iter().enumerate() {
            match stmt {
                Stmt::Local(local) => {
                    let Pat::Ident(pat) = &local.pat else {
                        return Err(syn::Error::new(
                            local.pat.span(),
                            "bind each chain to a plain name",
                        ));
                    };
                    let Some(init) = &local.init else {
                        return Err(syn::Error::new(
                            local.span(),
                            "each `let` must initialise a fluent chain",
                        ));
                    };
                    walker.walk_statement(&pat.ident, &init.expr, &builder)?;
                }
                Stmt::Expr(expr, None) if i + 1 == n_stmts => tail = Some(expr),
                other => {
                    return Err(syn::Error::new(
                        other.span(),
                        "graph bodies are straight-line `let name = <fluent chain>;` \
                         statements followed by the returned stream name(s)",
                    ));
                }
            }
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
            if !walker.index_of.contains_key(&name.to_string()) {
                return Err(syn::Error::new(
                    name.span(),
                    format!("`{name}` is not a stream bound by a `let` in this body"),
                ));
            }
        }

        Ok(GraphDef {
            name: sig.ident.clone(),
            builder_by_ref,
            outs: out_names.into_iter().zip(out_types).collect(),
            nodes: walker.nodes,
            wire_fn,
        })
    }
}

/// See the crate docs: fluent wiring in, `interpreted()` + `compiled()` out.
#[proc_macro]
pub fn graph(input: TokenStream) -> TokenStream {
    let def = parse_macro_input!(input as GraphDef);
    expand(&def).into()
}

fn expand(def: &GraphDef) -> TokenStream2 {
    let interpreted = expand_interpreted(def);
    let compiled = expand_compiled(def);
    let vis = &def.wire_fn.vis;
    let name = &def.name;
    // The user's function, verbatim, renamed `wire` and made pub within the
    // module — reusable as ordinary fluent wiring.
    let mut wire_fn = def.wire_fn.clone();
    wire_fn.sig.ident = Ident::new("wire", def.wire_fn.sig.ident.span());
    wire_fn.vis = parse_quote!(pub);
    quote! {
        #vis mod #name {
            #![allow(clippy::redundant_closure_call, clippy::let_and_return)]
            use super::*;
            use ::wingfoil_next::fluent::*;
            #wire_fn
            #interpreted
            #compiled
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

fn expand_compiled(def: &GraphDef) -> TokenStream2 {
    let n = def.nodes.len();
    let mut has_active_downstream = vec![false; n];
    for node in &def.nodes {
        for u in node.active_ups() {
            has_active_downstream[u] = true;
        }
    }

    let mut decls = Vec::new();
    let mut starts = Vec::new();
    let mut body = Vec::new();

    for (i, node) in def.nodes.iter().enumerate() {
        let name = &node.name;
        let cfg = format_ident!("__cfg_{}", name);
        let state = format_ident!("__state_{}", name);
        let value = format_ident!("__v_{}", name);
        let ticked = format_ident!("__t_{}", name);
        let exprs = &node.exprs;
        let idx = i;

        // cfg + state + value slot declarations.
        decls.push(match node.op {
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
        });

        if node.op.has_start() {
            let op_path = op_path(node.op);
            let state_arg = start_state_arg(node, &state);
            starts.push(quote! {
                {
                    let mut __ctx = ::wingfoil_next::op::Ctx::new(&mut __k, #idx);
                    #op_path::start(&mut #cfg, #state_arg, &mut __ctx);
                }
            });
        }

        // Dispatch condition.
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

        // The Op::cycle call with engine-supplied input.
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
        let call = quote! {
            {
                let mut __ctx = ::wingfoil_next::op::Ctx::new(&mut __k, #idx);
                match #cycle_call {
                    #on_value
                    ::wingfoil_next::op::Tick::Quiet => false,
                }
            }
        };

        let is_out = def.outs.iter().any(|(o, _)| o == name);
        body.push(if has_active_downstream[i] || is_out {
            quote! { let #ticked = (#cond) && #call; let _ = #ticked; }
        } else {
            quote! { if #cond { let _ = #call; } }
        });
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
            #(#decls)*
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

/// The concrete (inference-holed) op type.
fn op_type(op: OpKind) -> TokenStream2 {
    match op {
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
/// A ticker upstream has no value slot — its value is the unit literal.
fn cycle_input(def: &GraphDef, node: &NodeDef) -> TokenStream2 {
    let v = |ix: usize| -> TokenStream2 {
        if def.nodes[ix].op == OpKind::Ticker {
            quote! { &() }
        } else {
            let ident = format_ident!("__v_{}", def.nodes[ix].name);
            quote! { &#ident }
        }
    };
    let t = |ix: usize| format_ident!("__t_{}", def.nodes[ix].name);
    match node.op {
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

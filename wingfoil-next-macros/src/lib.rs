//! The `graph!` macro: **one wiring definition, two engines**.
//!
//! The macro parses a statement-form wiring DSL, builds the DAG at expansion
//! time, and emits a module containing:
//!
//! - `interpreted()` — the graph wired through the fluent API, returning a
//!   `Runner` plus a typed handle per declared output;
//! - `compiled(run_mode, run_for)` — a fully monomorphized runner: node
//!   state in locals, tick propagation as `bool`s, every `Op::cycle` call
//!   (closures included) inlined by the compiler.
//!
//! Both expansions are produced from the *same tokens*, so wiring and
//! compiled schedule cannot drift — the property the retrofitted codegen
//! needed fingerprints and re-supplied `Inputs` to approximate.
//!
//! ```ignore
//! wingfoil_next::graph! {
//!     mod evens_sum;
//!     out sum: u64;
//!     tick = ticker(Duration::from_micros(1));
//!     count = fold(tick, 0u64, |acc, _| *acc += 1);
//!     is_even = map(count, |i| i.is_multiple_of(2));
//!     evens = filter(count, is_even);
//!     sum = fold(evens, 0u64, |acc, v| *acc += v);
//! }
//!
//! let (mut runner, sum) = evens_sum::interpreted();
//! let compiled_sum = evens_sum::compiled(run_mode, run_for);
//! ```
//!
//! Supported ops: `ticker(period)`, `constant(value)`, `map(src, f)`,
//! `filter(src, cond)`, `fold(src, init, f)`, `sample(src, trigger)`,
//! `merge(a, b)`, `join(a, b, f)`, `delay(src, duration)`.
//!
//! Limitations (v1): graphs are defined at module scope, so closures can
//! reference items (`const`s, `fn`s) but not runtime locals; `external`
//! sources are not yet expressible (they need a handle out, which the
//! interpreted engine provides — use the fluent API for those graphs).

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Expr, Ident, Token, Type, Visibility, parse_macro_input};

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
    fn from_ident(ident: &Ident) -> syn::Result<Self> {
        Ok(match ident.to_string().as_str() {
            "ticker" => Self::Ticker,
            "constant" => Self::Constant,
            "map" => Self::Map,
            "filter" => Self::Filter,
            "fold" => Self::Fold,
            "sample" => Self::Sample,
            "merge" => Self::Merge,
            "join" => Self::Join,
            "delay" => Self::Delay,
            other => {
                return Err(syn::Error::new(
                    ident.span(),
                    format!(
                        "unknown op `{other}`; expected one of: ticker, constant, map, filter, \
                         fold, sample, merge, join, delay"
                    ),
                ));
            }
        })
    }

    /// (positions of node-reference args, total arity)
    fn shape(self) -> (&'static [usize], usize) {
        match self {
            Self::Ticker | Self::Constant => (&[], 1),
            Self::Map => (&[0], 2),
            Self::Filter => (&[0, 1], 2),
            Self::Fold => (&[0], 3),
            Self::Sample => (&[0, 1], 2),
            Self::Merge => (&[0, 1], 2),
            Self::Join => (&[0, 1], 3),
            Self::Delay => (&[0], 2),
        }
    }

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
    /// Indices (into the node list) of node-reference args, in arg order.
    refs: Vec<usize>,
    /// Non-node args (configs, closures), in arg order.
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
    vis: Visibility,
    name: Ident,
    outs: Vec<(Ident, Type)>,
    nodes: Vec<NodeDef>,
}

impl Parse for GraphDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let vis: Visibility = input.parse()?;
        input.parse::<Token![mod]>()?;
        let name: Ident = input.parse()?;
        input.parse::<Token![;]>()?;

        let mut outs: Vec<(Ident, Type)> = Vec::new();
        let mut nodes: Vec<NodeDef> = Vec::new();
        let mut index_of = std::collections::HashMap::<String, usize>::new();

        while !input.is_empty() {
            let ident: Ident = input.parse()?;
            if ident == "out" {
                let out_name: Ident = input.parse()?;
                input.parse::<Token![:]>()?;
                let ty: Type = input.parse()?;
                input.parse::<Token![;]>()?;
                outs.push((out_name, ty));
                continue;
            }
            // `name = op(args);`
            input.parse::<Token![=]>()?;
            let op_ident: Ident = input.parse()?;
            let op = OpKind::from_ident(&op_ident)?;
            let content;
            syn::parenthesized!(content in input);
            let args: Punctuated<Expr, Token![,]> =
                content.parse_terminated(Expr::parse, Token![,])?;
            input.parse::<Token![;]>()?;

            let (ref_positions, arity) = op.shape();
            if args.len() != arity {
                return Err(syn::Error::new(
                    op_ident.span(),
                    format!("`{op_ident}` takes {arity} argument(s), got {}", args.len()),
                ));
            }
            let mut refs = Vec::new();
            let mut exprs = Vec::new();
            for (pos, arg) in args.into_iter().enumerate() {
                if ref_positions.contains(&pos) {
                    let Expr::Path(p) = &arg else {
                        return Err(syn::Error::new_spanned(
                            &arg,
                            "expected the name of a previously defined node here",
                        ));
                    };
                    let Some(ref_name) = p.path.get_ident() else {
                        return Err(syn::Error::new_spanned(
                            &arg,
                            "expected a plain node name here",
                        ));
                    };
                    let Some(&ix) = index_of.get(&ref_name.to_string()) else {
                        return Err(syn::Error::new_spanned(
                            &arg,
                            format!("`{ref_name}` is not a node defined above this line"),
                        ));
                    };
                    refs.push(ix);
                } else {
                    exprs.push(arg);
                }
            }
            if index_of.insert(ident.to_string(), nodes.len()).is_some() {
                return Err(syn::Error::new(
                    ident.span(),
                    format!("node `{ident}` is defined twice"),
                ));
            }
            nodes.push(NodeDef {
                name: ident,
                op,
                refs,
                exprs,
            });
        }

        if outs.is_empty() {
            return Err(input.error("at least one `out name: Type;` is required"));
        }
        for (out_name, _) in &outs {
            if !index_of.contains_key(&out_name.to_string()) {
                return Err(syn::Error::new(
                    out_name.span(),
                    format!("output `{out_name}` is not a defined node"),
                ));
            }
        }
        Ok(GraphDef {
            vis,
            name,
            outs,
            nodes,
        })
    }
}

/// See the crate docs. One wiring definition; `interpreted()` and
/// `compiled()` emitted from the same tokens.
#[proc_macro]
pub fn graph(input: TokenStream) -> TokenStream {
    let def = parse_macro_input!(input as GraphDef);
    expand(&def).into()
}

fn expand(def: &GraphDef) -> TokenStream2 {
    let interpreted = expand_interpreted(def);
    let compiled = expand_compiled(def);
    let vis = &def.vis;
    let name = &def.name;
    quote! {
        #vis mod #name {
            #![allow(clippy::redundant_closure_call)]
            use super::*;
            #interpreted
            #compiled
        }
    }
}

fn expand_interpreted(def: &GraphDef) -> TokenStream2 {
    let mut stmts = Vec::new();
    for node in &def.nodes {
        let name = &node.name;
        let refs: Vec<&Ident> = node.refs.iter().map(|&ix| &def.nodes[ix].name).collect();
        let exprs = &node.exprs;
        let stmt = match node.op {
            OpKind::Ticker => quote! { let #name = __g.ticker(#(#exprs),*); },
            OpKind::Constant => quote! { let #name = __g.constant(#(#exprs),*); },
            OpKind::Map => {
                let (src,) = (&refs[0],);
                quote! { let #name = #src.map(#(#exprs),*); }
            }
            OpKind::Filter => {
                let (src, cond) = (&refs[0], &refs[1]);
                quote! { let #name = #src.filter(&#cond); }
            }
            OpKind::Fold => {
                let (src,) = (&refs[0],);
                quote! { let #name = #src.fold(#(#exprs),*); }
            }
            OpKind::Sample => {
                let (src, trigger) = (&refs[0], &refs[1]);
                quote! { let #name = #src.sample(&#trigger); }
            }
            OpKind::Merge => {
                let (a, b) = (&refs[0], &refs[1]);
                quote! { let #name = #a.merge(&#b); }
            }
            OpKind::Join => {
                let (a, b) = (&refs[0], &refs[1]);
                quote! { let #name = #a.join(&#b, #(#exprs),*); }
            }
            OpKind::Delay => {
                let (src,) = (&refs[0],);
                quote! { let #name = #src.delay(#(#exprs),*); }
            }
        };
        stmts.push(stmt);
    }
    let out_types: Vec<&Type> = def.outs.iter().map(|(_, t)| t).collect();
    let out_handles: Vec<TokenStream2> = def
        .outs
        .iter()
        .map(|(n, _)| quote! { #n.handle() })
        .collect();
    quote! {
        /// The graph wired through the fluent API: returns the runner plus a
        /// typed handle per declared output.
        pub fn interpreted() -> (
            ::wingfoil_next::interp::Runner,
            #(::wingfoil_next::interp::Handle<#out_types>,)*
        ) {
            let __g = ::wingfoil_next::fluent::GraphBuilder::new();
            #(#stmts)*
            let __runner = __g.build();
            (__runner, #(#out_handles,)*)
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
    for (out_name, _) in &def.outs {
        // Output values are read after the loop, which is fine either way —
        // but marking them keeps their `ticked` binding from being pruned
        // when they also happen to feed nothing.
        let _ = out_name;
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
            // at the cycle call so argument-position expectation drives its
            // signature inference (a closure bound to a plain local first
            // would fail E0282 against the generic Op bounds).
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
        /// visible to the compiler. Emitted from the same tokens as
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

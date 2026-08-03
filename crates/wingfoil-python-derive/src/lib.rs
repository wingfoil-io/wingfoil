//! `#[pyop]` — derive a Python-callable function from an `Op` impl.
//!
//! Placed on an `impl Op for MyOp` (alongside `#[op]`, or on its own), `#[pyop]`
//! re-emits the impl unchanged and generates a free `#[pyfunction]` that wires
//! the op onto a [`Stream`] at the erased boundary, via
//! `PyStream::wire_op1`. It is the proc-macro sugar over the `pyop_fn!`
//! declarative macro: instead of spelling the input/output/config types and the
//! step by hand, they are read off the `Op` impl's associated types and `cycle`.
//!
//! A user op becomes a **free function** `module.name(stream[, cfg])`, not a
//! `Stream` method — pyo3 forbids `#[pymethods]` on a foreign pyclass (the
//! polars-plugin shape). The generated function is named after `name`; register
//! it with `wrap_pyfunction!` in your `#[pymodule]`.
//!
//! ```ignore
//! use wingfoil_python::{pyop, Op, Tick, Activation, Ctx};
//!
//! struct Square;
//! #[pyop(name = square)]
//! impl Op for Square {
//!     type Cfg = ();
//!     type State = ();
//!     type In<'a> = (&'a f64,);
//!     type Out = f64;
//!     const ACTIVATION: Activation = Activation::NONE;
//!     fn cycle(_c: &mut (), _s: &mut (), input: (&f64,), _ctx: &mut Ctx)
//!         -> anyhow::Result<Tick<f64>> { Ok(Tick::Value(input.0 * input.0)) }
//! }
//! // => wingfoil.square(stream)   (register: wrap_pyfunction!(square, m)?)
//! ```
//!
//! **Scope:** one- to four-input concrete (non-generic) ops:
//!
//! | `In<'a>` | generated signature | seam |
//! |---|---|---|
//! | `(&'a A,)` | `module.name(stream)` | `wire_op1` |
//! | `(&'a A, &'a B)` | `module.name(stream, other)` | `wire_op2` |
//! | `(&'a A, &'a B, &'a C)` | `module.name(stream, second, third)` | `wire_op3` |
//! | `(&'a A, …, &'a D)` | `module.name(stream, second, third, fourth)` | `wire_op4` |
//!
//! All inputs are **active** (any one ticking runs the op); a passive edge
//! needs a hand-written method, as on the Rust side. The stream parameters are
//! named, so they can be passed by keyword.
//!
//! `Cfg` may be `()` (no config parameter), a single `FromPyObject` type
//! (`arg = <name>`, defaulting to `cfg`), or a **tuple** destructured into one
//! named parameter per element with `arg = (<p1>, <p2>, …)` — so an op with
//! `Cfg = (usize, f64)` reads as `name(stream, window, alpha)` rather than
//! taking a tuple. State may be any `Default`-seedable type (`State = ()` for
//! stateless, or e.g. `State = f64` for an accumulator — the engine re-seeds it
//! from `Default` on each run, so re-runs start clean).
//!
//! **Doc comments carry across.** All three macros here (`#[pyop]`,
//! `#[pygraph]`, `#[pyadapter]`) copy the annotated item's `///` docs onto the
//! generated `#[pyfunction]`, so they become the callable's Python docstring —
//! which is what `help()` prints and what the Sphinx reference in
//! `crates/wingfoil-python/docs/` renders. Write the prose once, in Rust,
//! aimed at the *Python* caller: name the Python argument shapes and say what a
//! tick carries. `#[pyop]` reads them off the `impl` block, `#[pygraph]` off
//! the wiring fn, `#[pyadapter]` off the free fn or the impl's method.
//!
//! Wider ops need a `register_op<n>`/`wire_op<n>` pair for their arity —
//! `register_op4` mirrors `register_op3` line for line — plus the parameter
//! name in `receiver_names`. The emitter itself is arity-generic, so nothing
//! else changes. (Each arity needs its own registration function because the
//! inputs are *heterogeneous* static types and Rust has no variadic generics;
//! a Python-authored node has no such limit — `Graph.custom_node` takes any
//! number of erased upstreams.)

use proc_macro::TokenStream;
use proc_macro2::{Delimiter, Group, TokenStream as TokenStream2, TokenTree};
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    Attribute, Error, FnArg, GenericArgument, Ident, ImplItem, Item, ItemFn, ItemImpl, Meta, Pat,
    PathArguments, ReturnType, Signature, Token, Type, parse_macro_input, parse_quote,
};

/// Parsed `#[pyop(name = <fn>, [arg = <cfg param> | arg = (<p1>, <p2>, ...)])]`.
///
/// `arg` names the generated function's config parameter(s). A single name
/// takes the whole `Cfg`; a parenthesised list destructures a **tuple** `Cfg`
/// into one named Python parameter per element, so an op configured by
/// `Cfg = (usize, f64)` reads as `name(stream, window, alpha)` rather than
/// `name(stream, (window, alpha))`.
struct PyOpArgs {
    name: Ident,
    args: Vec<Ident>,
}

impl Parse for PyOpArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut name = None;
        let mut args = Vec::new();
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "name" => name = Some(input.parse()?),
                "arg" => {
                    if input.peek(syn::token::Paren) {
                        let inner;
                        syn::parenthesized!(inner in input);
                        let names: Punctuated<Ident, Token![,]> =
                            Punctuated::parse_terminated(&inner)?;
                        if names.is_empty() {
                            return Err(Error::new(
                                key.span(),
                                "#[pyop] `arg = ()` names no parameters; drop it, or list one \
                                 name per tuple element of `Cfg`",
                            ));
                        }
                        args = names.into_iter().collect();
                    } else {
                        args = vec![input.parse()?];
                    }
                }
                other => {
                    return Err(Error::new(
                        key.span(),
                        format!("unknown #[pyop] key `{other}`; expected `name` or `arg`"),
                    ));
                }
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }
        let name =
            name.ok_or_else(|| Error::new(input.span(), "#[pyop] requires `name = <fn name>`"))?;
        Ok(PyOpArgs { name, args })
    }
}

#[proc_macro_attribute]
pub fn pyop(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as PyOpArgs);
    let imp = parse_macro_input!(item as ItemImpl);
    match expand(&args, &imp) {
        Ok(extra) => quote! { #imp #extra }.into(),
        Err(e) => {
            let e = e.to_compile_error();
            quote! { #imp #e }.into()
        }
    }
}

/// Parsed `#[pygraph(name = <py fn>)]`.
struct PyGraphArgs {
    name: Ident,
}

impl Parse for PyGraphArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        if key != "name" {
            return Err(Error::new(
                key.span(),
                "#[pygraph] requires `name = <py fn name>`",
            ));
        }
        input.parse::<Token![=]>()?;
        let name: Ident = input.parse()?;
        Ok(PyGraphArgs { name })
    }
}

/// `#[pygraph(name = ...)]` — expose a Rust-authored sub-graph wiring function
/// as a Python callable that splices the sub-graph's nodes into the caller's
/// builder and returns the erased output(s). Inputs and outputs edge-convert at
/// the boundary (`T: TryFrom<&PyElement>`, `U: Into<PyElement>`); the interior
/// runs at native types.
///
/// The wiring fn is `fn([&GraphBuilder,] &Stream<T>…) -> Stream<U> | (Stream<U>…)`:
///
/// - **N inputs** — each becomes a Python stream parameter, named by the same
///   convention `#[pyop]` uses (`stream`, then `other` at arity two, else
///   `second`/`third`/…), so they can be passed by keyword.
/// - **N outputs** — a tuple return becomes a Python tuple of streams, each
///   erased independently and each chainable.
/// - **An optional leading `&GraphBuilder`** — for wiring that creates nodes of
///   its own rather than only extending its inputs. The generated callable then
///   takes the graph first. This is what makes a `nitro!`-generated
///   **`nested()` compiled island** expressible: its signature is exactly
///   `(&GraphBuilder, &Stream<In>…) -> Stream<Out>`, so an island needs no
///   island-specific macro. With the builder and no stream inputs, the
///   sub-graph is a *source* and its outputs erase via `erase_source`.
///
/// The `name` must differ from the wiring function's own name (the macro emits a
/// `#[pyfunction]` of that name alongside the untouched function).
#[proc_macro_attribute]
pub fn pygraph(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as PyGraphArgs);
    let func = parse_macro_input!(item as ItemFn);
    match expand_pygraph(&args, &func) {
        Ok(extra) => quote! { #func #extra }.into(),
        Err(e) => {
            let e = e.to_compile_error();
            quote! { #func #e }.into()
        }
    }
}

/// The `X` of a `Stream<X>` (or `&Stream<X>`) type — the first generic argument
/// of the (optionally referenced) stream path, however that path is named or
/// qualified (`Stream`, `fluent::Stream`, an alias, …).
fn stream_inner(ty: &Type) -> syn::Result<Type> {
    let bare = match ty {
        Type::Reference(r) => &*r.elem,
        other => other,
    };
    if let Type::Path(p) = bare
        && let Some(seg) = p.path.segments.last()
        && let PathArguments::AngleBracketed(a) = &seg.arguments
        && let Some(GenericArgument::Type(t)) = a.args.first()
    {
        return Ok(t.clone());
    }
    Err(Error::new(
        ty.span(),
        "#[pygraph] expects a `&Stream<T>` argument and a `Stream<U>` return type",
    ))
}

/// Is this the wiring fn's optional leading `&GraphBuilder` parameter?
///
/// A sub-graph needs the builder when it creates nodes of its own rather than
/// only extending its inputs — most importantly a `nitro!`-generated
/// `nested()` **compiled island**, whose signature is
/// `(&GraphBuilder, &Stream<In>…) -> Stream<Out>`.
fn is_graph_builder(ty: &Type) -> bool {
    let bare = match ty {
        Type::Reference(r) => &*r.elem,
        other => other,
    };
    matches!(bare, Type::Path(p) if p
        .path
        .segments
        .last()
        .is_some_and(|seg| seg.ident == "GraphBuilder"))
}

fn expand_pygraph(args: &PyGraphArgs, func: &ItemFn) -> syn::Result<TokenStream2> {
    let fn_name = &func.sig.ident;
    let py_name = &args.name;
    if fn_name == py_name {
        return Err(Error::new(
            py_name.span(),
            "#[pygraph] `name` must differ from the function's own name (the macro emits a \
             `#[pyfunction]` of that name)",
        ));
    }

    let mut params = func.sig.inputs.iter();
    let mut peeked = params.clone();
    // An optional leading `&GraphBuilder`; the rest are the stream inputs.
    let takes_builder = matches!(peeked.next(), Some(FnArg::Typed(pt)) if is_graph_builder(&pt.ty));
    if takes_builder {
        params.next();
    }
    let in_tys = params
        .map(|arg| match arg {
            FnArg::Typed(pt) => stream_inner(&pt.ty),
            FnArg::Receiver(r) => Err(Error::new(
                r.span(),
                "#[pygraph] goes on a free wiring fn, not a method",
            )),
        })
        .collect::<syn::Result<Vec<_>>>()?;
    if in_tys.is_empty() && !takes_builder {
        return Err(Error::new(
            func.sig.span(),
            "#[pygraph] wiring fn takes `&Stream<T>` inputs, a leading `&GraphBuilder`, or both",
        ));
    }

    let out_ty = match &func.sig.output {
        ReturnType::Type(_, ty) => (**ty).clone(),
        ReturnType::Default => {
            return Err(Error::new(
                func.sig.span(),
                "#[pygraph] wiring fn must return `Stream<U>` or a tuple of streams",
            ));
        }
    };
    // One output, or a tuple of them — a sub-graph routinely produces several.
    let (out_tys, single_output) = match &out_ty {
        Type::Tuple(t) if !t.elems.is_empty() => (
            t.elems
                .iter()
                .map(stream_inner)
                .collect::<syn::Result<Vec<_>>>()?,
            false,
        ),
        other => (vec![stream_inner(other)?], true),
    };

    let receivers: Vec<Ident> = receiver_names(in_tys.len())
        .iter()
        .map(|s| Ident::new(s, py_name.span()))
        .collect();
    let receiver_params = receivers.iter().map(|r| {
        quote! { #r: pyo3::PyRef<'_, ::wingfoil_python::Stream> }
    });
    let graph_param = takes_builder.then(|| {
        quote! { graph: pyo3::PyRef<'_, ::wingfoil_python::Graph>, }
    });

    // Everything is spliced onto one graph, so any node on it can host the
    // conversions. With stream inputs the first is the anchor; with none (a
    // builder-only *source* sub-graph) the graph itself is, and its outputs
    // erase through `erase_source` rather than `erased_output`.
    let (anchor, erase): (TokenStream2, Vec<TokenStream2>) = if let Some(first) = receivers.first()
    {
        (
            quote! { #first.object() },
            out_tys
                .iter()
                .enumerate()
                .map(|(i, u)| {
                    let out = Ident::new(&format!("__out{i}"), py_name.span());
                    quote! { __anchor.erased_output::<#u>(#out) }
                })
                .collect(),
        )
    } else {
        (
            quote! { graph.object() },
            out_tys
                .iter()
                .enumerate()
                .map(|(i, u)| {
                    let out = Ident::new(&format!("__out{i}"), py_name.span());
                    quote! { __anchor.erase_source::<#u>(#out) }
                })
                .collect(),
        )
    };

    let typed_binds = receivers
        .iter()
        .zip(&in_tys)
        .enumerate()
        .map(|(i, (r, t))| {
            let typed = Ident::new(&format!("__typed{i}"), py_name.span());
            quote! { let #typed = #r.object().typed_input::<#t>(); }
        });
    let call_args = (0..in_tys.len()).map(|i| {
        let typed = Ident::new(&format!("__typed{i}"), py_name.span());
        quote! { &#typed }
    });
    // The builder always comes from the graph parameter (emitted whenever the
    // wiring fn takes it) — not from `__anchor`, which is a `PyStream` when
    // there are stream inputs and carries no builder.
    let builder_arg = takes_builder.then(|| quote! { graph.object().builder(), });

    let out_idents: Vec<Ident> = (0..out_tys.len())
        .map(|i| Ident::new(&format!("__out{i}"), py_name.span()))
        .collect();
    let bind_outs = if single_output {
        let out0 = &out_idents[0];
        quote! { let #out0 = #fn_name(#builder_arg #(#call_args),*); }
    } else {
        quote! { let (#(#out_idents),*) = #fn_name(#builder_arg #(#call_args),*); }
    };
    let (ret_ty, ret_expr) = if single_output {
        let e = &erase[0];
        (
            quote! { ::wingfoil_python::Stream },
            quote! { ::wingfoil_python::Stream::from(#e) },
        )
    } else {
        let tys = out_tys.iter().map(|_| quote! { ::wingfoil_python::Stream });
        (
            quote! { ( #(#tys),* ) },
            quote! { ( #(::wingfoil_python::Stream::from(#erase)),* ) },
        )
    };

    let docs = doc_attrs(&func.attrs);
    Ok(quote! {
        #[pyo3::pyfunction]
        #[allow(clippy::too_many_arguments)]
        #(#docs)*
        pub fn #py_name(
            #graph_param
            #(#receiver_params),*
        ) -> #ret_ty {
            let __anchor = #anchor;
            #(#typed_binds)*
            #bind_outs
            #ret_expr
        }
    })
}

/// Parsed `#[pyadapter(name = <py fn>, source)]`.
struct PyAdapterArgs {
    name: Ident,
    is_source: bool,
}

impl Parse for PyAdapterArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut name = None;
        let mut is_source = false;
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            match key.to_string().as_str() {
                "name" => {
                    input.parse::<Token![=]>()?;
                    name = Some(input.parse()?);
                }
                "source" => is_source = true,
                other => {
                    return Err(Error::new(
                        key.span(),
                        format!("unknown #[pyadapter] key `{other}`; expected `name` or `source`"),
                    ));
                }
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }
        let name = name.ok_or_else(|| {
            Error::new(input.span(), "#[pyadapter] requires `name = <py fn name>`")
        })?;
        Ok(PyAdapterArgs { name, is_source })
    }
}

/// `#[pyadapter(name = ...[, source])]` — expose adapter wiring as a Python
/// callable, edge-converting at the boundary.
///
/// It goes on a **free fn** whose first param is the receiver, or on an **impl**
/// block. Either way the `source` marker picks between the two shapes:
///
/// - **source** (`source` marker): receiver `&GraphBuilder`, returns
///   `Stream<T>` => `module.name(graph, args…)` — runs the wiring on the
///   caller's builder and erases the `T` output.
/// - **sink / transform** (no marker): receiver `&Stream<T>`, returns
///   `Stream<U>` => `module.name(stream, args…)` — extracts the input to native
///   `T`, runs the wiring, and erases the `U` output (a sink's `Stream<()>`
///   erases to Python `None`).
///
/// **Prefer the free-fn form when writing a binding.** The impl form's trait
/// exists only to give the macro a receiver, which costs a throwaway trait plus
/// a second copy of the whole signature; reach for it only when the adapter
/// genuinely wants a fluent Rust trait too. `name` must differ from the fn's own
/// name, since the macro emits a `#[pyfunction]` of that name beside it — so a
/// `postgres` binding module writes `fn read(…)` with `name = postgres_read`.
///
/// ```ignore
/// #[pyadapter(name = kafka_read, source)]
/// #[pyo3(signature = (brokers, topic, buffer_size = None))]
/// fn read(g: &GraphBuilder, brokers: String, topic: String, buffer_size: Option<usize>)
///     -> anyhow::Result<Stream<Burst<KafkaRecord>>> { /* … */ }
/// // => wingfoil.kafka_read(graph, brokers, topic, buffer_size=None)
/// ```
///
/// **Burst shapes** are handled too: a `Stream<Burst<T>>` erases to a Python
/// `list` per tick (same-instant values grouped), and on the way *in* a Python
/// `list`/`tuple` rebuilds a multi-value burst (any other value → a
/// single-element burst) — so a burst source round-trips into a burst sink. So
/// `Stream<Burst<T>>` may appear as a source's return, a sink's `Self`, or a
/// transform's output. Adapter params become `#[pyfunction]` params, so they
/// must be `FromPyObject`.
///
/// **Fallible wiring** is supported: a method returning `Result<Stream<T>>`
/// (any path ending in `Result`, so `anyhow::Result<..>` too) generates a
/// `PyResult`-returning `#[pyfunction]`, mapping the wiring error to a Python
/// exception. That is the usual shape for a real adapter, which validates its
/// run window / run mode / config at wiring time.
#[proc_macro_attribute]
pub fn pyadapter(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as PyAdapterArgs);
    let item = parse_macro_input!(item as Item);
    // `#[pyo3(..)]` attributes on the adapter fn are *for the generated
    // `#[pyfunction]`* — strip them from the re-emitted item, where they would
    // be an unknown attribute.
    match item {
        Item::Impl(mut imp) => {
            let expanded = expand_pyadapter_impl(&args, &imp);
            for it in &mut imp.items {
                if let ImplItem::Fn(f) = it {
                    f.attrs.retain(|a| !a.path().is_ident("pyo3"));
                }
            }
            emit(quote! { #imp }, expanded)
        }
        Item::Fn(mut func) => {
            let expanded = expand_pyadapter_fn(&args, &func);
            func.attrs.retain(|a| !a.path().is_ident("pyo3"));
            // An adapter's arity is dictated by its knobs (a time-sliced reader
            // legitimately takes ~8), so the wiring fn gets the same allow the
            // generated one does rather than every binding repeating it.
            func.attrs
                .push(parse_quote!(#[allow(clippy::too_many_arguments)]));
            emit(quote! { #func }, expanded)
        }
        other => Error::new(
            other.span(),
            "#[pyadapter] goes on a wiring `fn` (first param the receiver) or on an \
             `impl Trait for GraphBuilder | Stream<T>`",
        )
        .to_compile_error()
        .into(),
    }
}

/// Re-emit the annotated item followed by the generated `#[pyfunction]` (or the
/// compile error, so the original item still resolves for downstream code).
fn emit(item: TokenStream2, expanded: syn::Result<TokenStream2>) -> TokenStream {
    match expanded {
        Ok(extra) => quote! { #item #extra }.into(),
        Err(e) => {
            let e = e.to_compile_error();
            quote! { #item #e }.into()
        }
    }
}

/// How the generated `#[pyfunction]` reaches the user's wiring code.
enum Callee {
    /// An `impl` block's method — called *on* the receiver.
    Method(Ident),
    /// A free fn — the receiver is its first argument.
    Free(Ident),
}

impl Callee {
    /// The call expression. The two forms take the receiver differently — a
    /// method call binds it as `self`, a free call passes it as the first
    /// argument — so each site supplies both spellings (they differ for a sink,
    /// where the method form uses the owned stream and the free form borrows it).
    fn call(
        &self,
        method_recv: TokenStream2,
        free_recv: TokenStream2,
        args: &[Ident],
    ) -> TokenStream2 {
        match self {
            Callee::Method(name) => quote! { #method_recv.#name(#(#args),*) },
            Callee::Free(name) => quote! { #name(#free_recv #(, #args)*) },
        }
    }
}

/// The free-fn form: `fn m(recv, args…) -> Stream<U> | Result<Stream<U>>`, where
/// `recv` is `&GraphBuilder` (source) or `&Stream<T>` (sink/transform).
///
/// This is the form a *binding* module wants — the `impl` form's trait exists
/// only to give the macro a receiver, and costs a throwaway trait plus a second
/// copy of every signature.
fn expand_pyadapter_fn(args: &PyAdapterArgs, func: &ItemFn) -> syn::Result<TokenStream2> {
    let fn_name = &func.sig.ident;
    if fn_name == &args.name {
        return Err(Error::new(
            args.name.span(),
            "#[pyadapter] `name` must differ from the wiring fn's own name (the macro emits a \
             `#[pyfunction]` of that name alongside it) — e.g. `fn read` with `name = kafka_read`",
        ));
    }
    let mut inputs = func.sig.inputs.iter();
    let receiver = inputs.next().ok_or_else(|| {
        Error::new(
            func.sig.span(),
            "#[pyadapter] wiring fn takes the receiver as its first param: `&GraphBuilder` for a \
             source, `&Stream<T>` for a sink/transform",
        )
    })?;
    let receiver_ty = match receiver {
        FnArg::Typed(pt) => (*pt.ty).clone(),
        FnArg::Receiver(r) => {
            return Err(Error::new(
                r.span(),
                "#[pyadapter] wiring fn takes the receiver as a normal first param, not `self`",
            ));
        }
    };
    // Only the sink form needs the receiver's type (to build the typed input);
    // a source's receiver is always the builder.
    let in_ty = if args.is_source {
        None
    } else {
        Some(stream_inner(&receiver_ty)?)
    };
    let (param_decls, param_names) = split_params(inputs)?;
    let out_ty = return_type(&func.sig)?;
    emit_pyadapter(
        args,
        &Callee::Free(fn_name.clone()),
        in_ty,
        out_ty,
        &param_decls,
        &param_names,
        &forwarded_attrs(&func.attrs),
    )
}

/// The `impl` form: `impl Trait for GraphBuilder | Stream<T> { fn m(&self, …) }`.
/// Kept for adapters that genuinely want a fluent Rust trait; a binding module
/// should prefer the free-fn form above.
fn expand_pyadapter_impl(args: &PyAdapterArgs, imp: &ItemImpl) -> syn::Result<TokenStream2> {
    // The single adapter method in the impl.
    let mut methods = imp.items.iter().filter_map(|it| match it {
        ImplItem::Fn(f) => Some(f),
        _ => None,
    });
    let method = methods.next().ok_or_else(|| {
        Error::new(
            imp.span(),
            "#[pyadapter] impl must contain the adapter method",
        )
    })?;
    if methods.next().is_some() {
        return Err(Error::new(
            imp.span(),
            "#[pyadapter] v1 expects exactly one adapter method in the impl",
        ));
    }
    let in_ty = if args.is_source {
        None
    } else {
        Some(stream_inner(&imp.self_ty)?)
    };
    let (param_decls, param_names) = split_params(method.sig.inputs.iter())?;
    let out_ty = return_type(&method.sig)?;
    emit_pyadapter(
        args,
        &Callee::Method(method.sig.ident.clone()),
        in_ty,
        out_ty,
        &param_decls,
        &param_names,
        &forwarded_attrs(&method.attrs),
    )
}

/// The attributes forwarded from the annotated item to the generated
/// `#[pyfunction]`:
///
/// * `#[pyo3(..)]` — how an adapter declares its Python defaults, e.g.
///   `#[pyo3(signature = (conn, chunk_secs = 3600, buffer_size = None))]`.
/// * `#[doc]` (i.e. `///`) — the adapter's prose *is* its Python docstring, and
///   therefore its entry in the generated Sphinx reference. Dropping it would
///   leave every bound adapter undocumented on the Python side while the Rust
///   source next to it carries the full explanation.
fn forwarded_attrs(attrs: &[Attribute]) -> Vec<Attribute> {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("pyo3") || a.path().is_ident("doc"))
        .cloned()
        .collect()
}

/// The `#[doc]` attributes of an item, for the generated functions that take no
/// `#[pyo3(..)]` forwarding ([`pygraph`] and [`pyop`]).
fn doc_attrs(attrs: &[Attribute]) -> Vec<Attribute> {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .cloned()
        .collect()
}

/// Split an adapter fn's non-receiver params into `(decls, names)`, forwarded to
/// the generated `#[pyfunction]` verbatim. A `self` receiver is skipped (the
/// `impl` form); the free-fn form has already consumed its first param.
type Params = (Vec<TokenStream2>, Vec<Ident>);

fn split_params<'a>(inputs: impl Iterator<Item = &'a FnArg>) -> syn::Result<Params> {
    let mut decls = Vec::new();
    let mut names = Vec::new();
    for arg in inputs {
        match arg {
            FnArg::Receiver(_) => {}
            FnArg::Typed(pt) => {
                let name = match &*pt.pat {
                    Pat::Ident(pi) => pi.ident.clone(),
                    _ => {
                        return Err(Error::new(
                            pt.pat.span(),
                            "#[pyadapter] adapter params must be simple identifiers",
                        ));
                    }
                };
                let ty = &pt.ty;
                decls.push(quote! { #name: #ty });
                names.push(name);
            }
        }
    }
    Ok((decls, names))
}

/// The adapter fn's declared return type.
fn return_type(sig: &Signature) -> syn::Result<Type> {
    match &sig.output {
        ReturnType::Type(_, ty) => Ok((**ty).clone()),
        ReturnType::Default => Err(Error::new(
            sig.span(),
            "#[pyadapter] adapter fn must return `Stream<T>` or `Result<Stream<T>>`",
        )),
    }
}

/// Emit the `#[pyfunction]` — shared by both forms, which differ only in how
/// they reach the user's wiring ([`Callee`]) and where the sink's input type
/// comes from.
fn emit_pyadapter(
    args: &PyAdapterArgs,
    callee: &Callee,
    in_ty: Option<Type>,
    out_ty: Type,
    param_decls: &[TokenStream2],
    param_names: &[Ident],
    raw_py_attrs: &[Attribute],
) -> syn::Result<TokenStream2> {
    // A real adapter's wiring is usually **fallible** (`postgres_read` validates
    // the run window, `postgres_sub` rejects a historical run, a sink quotes and
    // checks its table name), so it may return `Result<Stream<T>>`. When it does,
    // the generated `#[pyfunction]` returns `PyResult<Stream>` and the wiring
    // error surfaces as a Python exception instead of aborting the run; an
    // infallible `Stream<T>` keeps the plain return.
    let (out_ty, fallible) = match result_inner(&out_ty) {
        Some(inner) => (inner, true),
        None => (out_ty, false),
    };
    let py_name = &args.name;
    // One output stream, or a **tuple** of them — the `(data, status)` shape a
    // live source with a connection-status stream returns (`zmq_sub`), and the
    // same spelling `#[pygraph]` already accepts. A tuple erases element-wise
    // into a Python tuple of `Stream`s.
    let (out_inners, is_tuple) = match &out_ty {
        Type::Tuple(t) if !t.elems.is_empty() => (
            t.elems
                .iter()
                .map(stream_inner)
                .collect::<syn::Result<Vec<_>>>()?,
            true,
        ),
        other => (vec![stream_inner(other)?], false),
    };

    let stream_ty = quote! { ::wingfoil_python::Stream };
    let ret_core = if is_tuple {
        let each = out_inners.iter().map(|_| stream_ty.clone());
        quote! { ( #(#each),* ) }
    } else {
        stream_ty.clone()
    };
    let (ret_ty, unwrap) = if fallible {
        (
            quote! { pyo3::PyResult<#ret_core> },
            quote! { .map_err(::wingfoil_python::to_pyerr)? },
        )
    } else {
        (ret_core, quote! {})
    };

    // Bind the wiring call's result: one name, or a destructured tuple.
    let bind_outputs = |slot: &str, call: TokenStream2, unwrap: &TokenStream2| {
        if is_tuple {
            let names: Vec<Ident> = (0..out_inners.len())
                .map(|i| Ident::new(&format!("{slot}{i}"), py_name.span()))
                .collect();
            quote! { let ( #(#names),* ) = #call #unwrap; }
        } else {
            let name = Ident::new(slot, py_name.span());
            quote! { let #name = #call #unwrap; }
        }
    };
    // The name holding output `i` — `__slot` for a single, `__slot{i}` for a
    // tuple element.
    let slot_name = |slot: &str, i: usize| {
        let n = if is_tuple {
            format!("{slot}{i}")
        } else {
            slot.to_string()
        };
        Ident::new(&n, py_name.span())
    };
    // Assemble the return value from the per-output erase expressions.
    let wrap_ret = |erased: Vec<TokenStream2>| {
        let built: Vec<TokenStream2> = erased
            .into_iter()
            .map(|e| quote! { ::wingfoil_python::Stream::from(#e) })
            .collect();
        let value = if is_tuple {
            quote! { ( #(#built),* ) }
        } else {
            let only = &built[0];
            quote! { #only }
        };
        if fallible {
            quote! { Ok(#value) }
        } else {
            value
        }
    };
    // The generated fn takes the graph/stream as its first param, so a forwarded
    // `signature` — written by the author over their *own* params — gains it too.
    let receiver_name = Ident::new(
        if args.is_source { "graph" } else { "stream" },
        py_name.span(),
    );
    let py_attrs = forward_pyo3_attrs(raw_py_attrs, &receiver_name);
    // An adapter's knobs plus the generated receiver routinely exceed clippy's
    // argument threshold, and the author cannot annotate generated code.
    let preamble = quote! {
        #[pyo3::pyfunction]
        #[allow(clippy::too_many_arguments)]
        #(#py_attrs)*
    };

    if args.is_source {
        // Source: `(&GraphBuilder, args…) -> Stream<T> | Stream<Burst<T>>` =>
        // `module.name(graph, args…)`: run the adapter on the caller's builder,
        // erase the output (a `Burst<T>` becomes a Python list).
        let call = callee.call(
            quote! { __obj.builder() },
            quote! { __obj.builder() },
            param_names,
        );
        let bind = bind_outputs("__typed", call, &unwrap);
        let erased = out_inners
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let src = slot_name("__typed", i);
                match burst_inner(t) {
                    Some(b) => quote! { __obj.erase_burst_source::<#b>(#src) },
                    None => quote! { __obj.erase_source::<#t>(#src) },
                }
            })
            .collect();
        let body = wrap_ret(erased);
        Ok(quote! {
            #preamble
            pub fn #py_name(
                graph: pyo3::PyRef<'_, ::wingfoil_python::Graph>,
                #(#param_decls),*
            ) -> #ret_ty {
                let __obj = graph.object();
                #bind
                #body
            }
        })
    } else {
        // Sink / transform: `(&Stream<T>, args…) -> Stream<U> | Stream<Burst<U>>`
        // => `module.name(stream, args…)`: extract the input to native `T` (a
        // burst receiver rebuilds a burst from each Python list/tuple, else a
        // single-element burst), run the adapter, erase the output (a sink's
        // `Stream<()>` erases to Python `None`; a `Burst<U>` to a list).
        let in_inner = in_ty.expect("invariant: the sink form always resolves an input type");
        let typed_in = match burst_inner(&in_inner) {
            Some(t) => quote! { __obj.typed_burst_input::<#t>() },
            None => quote! { __obj.typed_input::<#in_inner>() },
        };
        let call = callee.call(quote! { __typed }, quote! { &__typed }, param_names);
        let bind = bind_outputs("__out", call, &unwrap);
        let erased = out_inners
            .iter()
            .enumerate()
            .map(|(i, u)| {
                let src = slot_name("__out", i);
                match burst_inner(u) {
                    Some(b) => quote! { __obj.erased_burst_output::<#b>(#src) },
                    None => quote! { __obj.erased_output::<#u>(#src) },
                }
            })
            .collect();
        let body = wrap_ret(erased);
        Ok(quote! {
            #preamble
            pub fn #py_name(
                stream: pyo3::PyRef<'_, ::wingfoil_python::Stream>,
                #(#param_decls),*
            ) -> #ret_ty {
                let __obj = stream.object();
                let __typed = #typed_in;
                #bind
                #body
            }
        })
    }
}

/// Forward the adapter method's `#[pyo3(..)]` attributes to the generated
/// `#[pyfunction]`, prepending `receiver` to any `signature = (..)` list.
///
/// The author writes the signature over the *adapter method's* own params
/// (`#[pyo3(signature = (conn_str, chunk_secs = 3600))]`); the generated function
/// additionally takes the graph/stream as its first param, and pyo3 requires a
/// signature to name every param. Injecting it here keeps the generated receiver
/// out of the author's hands.
fn forward_pyo3_attrs(attrs: &[Attribute], receiver: &Ident) -> Vec<Attribute> {
    attrs
        .iter()
        .map(|attr| {
            let Meta::List(list) = &attr.meta else {
                return attr.clone();
            };
            let mut out = TokenStream2::new();
            // Track `signature` `=` `( .. )` so only that group is rewritten.
            let (mut saw_signature, mut saw_eq) = (false, false);
            for tt in list.tokens.clone() {
                match &tt {
                    TokenTree::Ident(ident) if ident == "signature" => {
                        saw_signature = true;
                        saw_eq = false;
                        out.extend([tt]);
                    }
                    TokenTree::Punct(p) if saw_signature && p.as_char() == '=' => {
                        saw_eq = true;
                        out.extend([tt]);
                    }
                    TokenTree::Group(g)
                        if saw_signature && saw_eq && g.delimiter() == Delimiter::Parenthesis =>
                    {
                        let inner = g.stream();
                        let merged = if inner.is_empty() {
                            quote! { #receiver }
                        } else {
                            quote! { #receiver, #inner }
                        };
                        out.extend([TokenTree::Group(Group::new(Delimiter::Parenthesis, merged))]);
                        saw_signature = false;
                        saw_eq = false;
                    }
                    _ => out.extend([tt]),
                }
            }
            let path = &list.path;
            parse_quote!(#[#path(#out)])
        })
        .collect()
}

/// If `ty` is `Result<X>` / `anyhow::Result<X>` / `Result<X, E>` (last path
/// segment literally `Result`), the `X`; else `None`. Distinguishes a fallible
/// adapter wiring fn — the common shape for real adapters, which validate their
/// config and run mode at wiring — from an infallible one.
fn result_inner(ty: &Type) -> Option<Type> {
    if let Type::Path(p) = ty
        && let Some(seg) = p.path.segments.last()
        && seg.ident == "Result"
        && let PathArguments::AngleBracketed(a) = &seg.arguments
        && let Some(GenericArgument::Type(t)) = a.args.first()
    {
        return Some(t.clone());
    }
    None
}

/// If `ty` is `Burst<X>` (last path segment literally `Burst`), the `X`; else
/// `None`. Distinguishes a burst-shaped adapter stream from a single-value one.
fn burst_inner(ty: &Type) -> Option<Type> {
    if let Type::Path(p) = ty
        && let Some(seg) = p.path.segments.last()
        && seg.ident == "Burst"
        && let PathArguments::AngleBracketed(a) = &seg.arguments
        && let Some(GenericArgument::Type(t)) = a.args.first()
    {
        return Some(t.clone());
    }
    None
}

/// The `T`s of a reference tuple `(&'a T, …)` — one entry per op input. `#[pyop]`
/// supports one- or two-input ops.
/// The generated function's stream parameter names, by input arity — shared by
/// `#[pyop]` and `#[pygraph]` so one convention covers both.
///
/// `other` (rather than `second`) at arity two is the name that shipped with the
/// two-input `#[pyop]`; keeping it avoids breaking a caller who passes it by
/// keyword.
fn receiver_names(n: usize) -> Vec<String> {
    const ORDINALS: [&str; 5] = ["second", "third", "fourth", "fifth", "sixth"];
    if n == 2 {
        return vec!["stream".to_string(), "other".to_string()];
    }
    std::iter::once("stream".to_string())
        .chain((1..n).map(|i| {
            ORDINALS
                .get(i - 1)
                .map(|s| (*s).to_string())
                .unwrap_or_else(|| format!("input{i}"))
        }))
        .collect()
}

/// The generated function's config parameter list and the value handed to the
/// op, from `Cfg` and the `arg = …` names.
///
/// Three shapes: a unit `Cfg` contributes nothing; a single name takes the
/// whole `Cfg` (defaulting to `cfg`); a parenthesised list destructures a tuple
/// `Cfg` element-wise, so each knob is its own named Python argument and the
/// caller writes `name(stream, window, alpha)` instead of passing a tuple.
fn cfg_params(
    args: &PyOpArgs,
    cfg_ty: &Type,
    span: proc_macro2::Span,
) -> syn::Result<(TokenStream2, TokenStream2)> {
    if is_unit(cfg_ty) {
        if !args.args.is_empty() {
            return Err(Error::new(
                span,
                "#[pyop] `arg` names a config parameter, but this op has `type Cfg = ()`",
            ));
        }
        return Ok((quote! {}, quote! { () }));
    }
    if args.args.len() <= 1 {
        let arg = args
            .args
            .first()
            .cloned()
            .unwrap_or_else(|| Ident::new("cfg", span));
        return Ok((quote! { , #arg: #cfg_ty }, quote! { #arg }));
    }
    // Several names: `Cfg` must be a tuple of matching arity, and each element
    // becomes its own parameter.
    let Type::Tuple(tuple) = cfg_ty else {
        return Err(Error::new(
            span,
            format!(
                "#[pyop] `arg` lists {} names, so `Cfg` must be a tuple of {} elements; \
                 it is not a tuple — use a single `arg = <name>`",
                args.args.len(),
                args.args.len(),
            ),
        ));
    };
    if tuple.elems.len() != args.args.len() {
        return Err(Error::new(
            span,
            format!(
                "#[pyop] `arg` lists {} names but `Cfg` is a tuple of {} elements — \
                 give one name per element",
                args.args.len(),
                tuple.elems.len(),
            ),
        ));
    }
    let names = &args.args;
    let tys = tuple.elems.iter();
    Ok((quote! { #(, #names: #tys)* }, quote! { ( #(#names),* ) }))
}

fn ref_tuple_elems(ty: &Type) -> syn::Result<Vec<Type>> {
    if let Type::Tuple(t) = ty {
        let mut elems = Vec::with_capacity(t.elems.len());
        for e in &t.elems {
            match e {
                Type::Reference(r) => elems.push((*r.elem).clone()),
                _ => {
                    return Err(Error::new(
                        e.span(),
                        "#[pyop] op inputs must be references (`&'a T`)",
                    ));
                }
            }
        }
        if matches!(elems.len(), 1..=4) {
            return Ok(elems);
        }
    }
    Err(Error::new(
        ty.span(),
        "#[pyop] supports one- to four-input ops (`type In<'a> = (&'a A,)` through \
         `(&'a A, &'a B, &'a C, &'a D)`); a wider op needs a `register_op<n>`/`wire_op<n>` \
         pair for its arity — add them the way `register_op4` mirrors `register_op3`, then \
         add the parameter name to `receiver_names`",
    ))
}

fn is_unit(ty: &Type) -> bool {
    matches!(ty, Type::Tuple(t) if t.elems.is_empty())
}

fn expand(args: &PyOpArgs, imp: &ItemImpl) -> syn::Result<TokenStream2> {
    let self_ty = &*imp.self_ty;

    if imp.generics.type_params().next().is_some() {
        return Err(Error::new(
            imp.generics.span(),
            "#[pyop] requires a concrete Op impl (no generic type parameters); \
             instantiate the op at a concrete type",
        ));
    }

    let (mut in_ty, mut out_ty, mut cfg_ty, mut state_ty) = (None, None, None, None);
    for it in &imp.items {
        if let ImplItem::Type(t) = it {
            match t.ident.to_string().as_str() {
                "In" => in_ty = Some(t.ty.clone()),
                "Out" => out_ty = Some(t.ty.clone()),
                "Cfg" => cfg_ty = Some(t.ty.clone()),
                "State" => state_ty = Some(t.ty.clone()),
                _ => {}
            }
        }
    }
    let missing = |w: &str| Error::new(imp.span(), format!("#[pyop]: Op impl missing `type {w}`"));
    let in_ty = in_ty.ok_or_else(|| missing("In"))?;
    let out_ty = out_ty.ok_or_else(|| missing("Out"))?;
    let cfg_ty = cfg_ty.ok_or_else(|| missing("Cfg"))?;
    let state_ty = state_ty.ok_or_else(|| missing("State"))?;

    let elems = ref_tuple_elems(&in_ty)?;

    let name = &args.name;
    let name_str = name.to_string();

    let (param, cfg_value) = cfg_params(args, &cfg_ty, name.span())?;
    let state_seed = quote! { || <#state_ty as ::core::default::Default>::default() };

    // One emitter for every arity: the arms differed only in how many streams
    // they took and which `wire_op<n>` they called, so they are derived rather
    // than pasted — adding a rung is `register_op<n>` + `wire_op<n>` plus a name
    // in `RECEIVER_NAMES`.
    let n = elems.len();
    let wire = format_ident!("wire_op{}", n);
    let receivers: Vec<Ident> = receiver_names(n)
        .iter()
        .map(|s| Ident::new(s, name.span()))
        .collect();
    let receiver_params = receivers.iter().map(|r| {
        quote! { #r: pyo3::PyRef<'_, ::wingfoil_python::Stream> }
    });
    let receiver_args = receivers.iter().map(|r| quote! { #r.object() });
    let input_idents: Vec<Ident> = (0..n)
        .map(|i| Ident::new(&format!("__a{i}"), name.span()))
        .collect();
    let closure_params = input_idents
        .iter()
        .zip(&elems)
        .map(|(id, ty)| quote! { #id: &#ty });

    let docs = doc_attrs(&imp.attrs);
    let body = quote! {
        #[pyo3::pyfunction]
        #(#docs)*
        fn #name(
            #(#receiver_params),*
            #param
        ) -> ::wingfoil_python::Stream {
            ::wingfoil_python::Stream::from(
                ::wingfoil_python::PyStream::#wire::<
                    #(#elems,)* _, _, #out_ty, _, _,
                >(
                    #(#receiver_args,)*
                    #name_str,
                    <#self_ty as ::wingfoil_python::Op>::ACTIVATION,
                    #cfg_value,
                    #state_seed,
                    |__c, __s, #(#closure_params,)* __ctx| {
                        <#self_ty as ::wingfoil_python::Op>::cycle(
                            __c, __s, (#(#input_idents),*,), __ctx,
                        )
                    },
                )
            )
        }
    };
    Ok(body)
}

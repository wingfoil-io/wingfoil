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
//! use wingfoil_next_python::{pyop, Op, Tick, Activation, Ctx};
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
//! // => wingfoil_next.square(stream)   (register: wrap_pyfunction!(square, m)?)
//! ```
//!
//! **Scope:** one- or two-input (`In<'a> = (&'a A,)` → `module.name(stream)`,
//! `(&'a A, &'a B)` → `module.name(stream, other)`), concrete (non-generic)
//! ops, with `Cfg = ()` or a single `FromPyObject` type. State may be any
//! `Default`-seedable type (`State = ()` for stateless, or e.g. `State = f64`
//! for an accumulator — the engine re-seeds it from `Default` on each run, so
//! re-runs start clean). Ops with 3+ inputs use `PyStream::wire_op1`/`wire_op2`
//! (or the object form) directly.

use proc_macro::TokenStream;
use proc_macro2::{Delimiter, Group, TokenStream as TokenStream2, TokenTree};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    Attribute, Error, FnArg, GenericArgument, Ident, ImplItem, Item, ItemFn, ItemImpl, Meta, Pat,
    PathArguments, ReturnType, Signature, Token, Type, parse_macro_input, parse_quote,
};

/// Parsed `#[pyop(name = <fn>, [arg = <cfg param>])]`.
struct PyOpArgs {
    name: Ident,
    arg: Option<Ident>,
}

impl Parse for PyOpArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut name = None;
        let mut arg = None;
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let val: Ident = input.parse()?;
            match key.to_string().as_str() {
                "name" => name = Some(val),
                "arg" => arg = Some(val),
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
        Ok(PyOpArgs { name, arg })
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
/// (`fn(&Stream<T>) -> Stream<U>`) as a Python callable `module.name(stream)`
/// that splices the sub-graph's nodes into the caller's builder and returns the
/// erased output. `T`/`U` edge-convert at the boundary (`T: TryFrom<&PyElement>`,
/// `U: Into<PyElement>`); the interior runs at native types.
///
/// The `name` must differ from the wiring function's own name (the macro emits a
/// `#[pyfunction]` of that name alongside the untouched function). Single-input,
/// single-output in v1.
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
    if func.sig.inputs.len() != 1 {
        return Err(Error::new(
            func.sig.span(),
            "#[pygraph] supports a single-input wiring fn (`fn(&Stream<T>) -> Stream<U>`) in v1",
        ));
    }
    let in_ty = match func.sig.inputs.first() {
        Some(FnArg::Typed(pt)) => &*pt.ty,
        _ => {
            return Err(Error::new(
                func.sig.span(),
                "#[pygraph] wiring fn takes a `&Stream<T>` argument",
            ));
        }
    };
    let t_ty = stream_inner(in_ty)?;
    let out_ty = match &func.sig.output {
        ReturnType::Type(_, ty) => &**ty,
        ReturnType::Default => {
            return Err(Error::new(
                func.sig.span(),
                "#[pygraph] wiring fn must return `Stream<U>`",
            ));
        }
    };
    let u_ty = stream_inner(out_ty)?;

    Ok(quote! {
        #[pyo3::pyfunction]
        fn #py_name(
            stream: pyo3::PyRef<'_, ::wingfoil_next_python::Stream>,
        ) -> ::wingfoil_next_python::Stream {
            let __obj = stream.object();
            let __typed = __obj.typed_input::<#t_ty>();
            let __out = #fn_name(&__typed);
            ::wingfoil_next_python::Stream::from(__obj.erased_output::<#u_ty>(__out))
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
/// // => wingfoil_next.kafka_read(graph, brokers, topic, buffer_size=None)
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
        &pyo3_attrs(&func.attrs),
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
        &pyo3_attrs(&method.attrs),
    )
}

/// The `#[pyo3(..)]` attributes written on the adapter fn — forwarded to the
/// generated `#[pyfunction]`. That is how an adapter declares Python defaults,
/// e.g. `#[pyo3(signature = (conn, chunk_secs = 3600, buffer_size = None))]`.
fn pyo3_attrs(attrs: &[Attribute]) -> Vec<Attribute> {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("pyo3"))
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
    let out_inner = stream_inner(&out_ty)?;
    let py_name = &args.name;

    let (ret_ty, unwrap) = if fallible {
        (
            quote! { pyo3::PyResult<::wingfoil_next_python::Stream> },
            quote! { .map_err(::wingfoil_next_python::to_pyerr)? },
        )
    } else {
        (quote! { ::wingfoil_next_python::Stream }, quote! {})
    };
    // `Stream::from(..)`, wrapped in `Ok(..)` when the signature is fallible.
    let wrap_ret = |expr: TokenStream2| {
        let built = quote! { ::wingfoil_next_python::Stream::from(#expr) };
        if fallible {
            quote! { Ok(#built) }
        } else {
            built
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
        let erase = match burst_inner(&out_inner) {
            Some(t) => quote! { __obj.erase_burst_source::<#t>(__typed) },
            None => quote! { __obj.erase_source::<#out_inner>(__typed) },
        };
        let call = callee.call(
            quote! { __obj.builder() },
            quote! { __obj.builder() },
            param_names,
        );
        let body = wrap_ret(erase);
        Ok(quote! {
            #preamble
            pub fn #py_name(
                graph: pyo3::PyRef<'_, ::wingfoil_next_python::Graph>,
                #(#param_decls),*
            ) -> #ret_ty {
                let __obj = graph.object();
                let __typed = #call #unwrap;
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
        let erase = match burst_inner(&out_inner) {
            Some(u) => quote! { __obj.erased_burst_output::<#u>(__out) },
            None => quote! { __obj.erased_output::<#out_inner>(__out) },
        };
        let call = callee.call(quote! { __typed }, quote! { &__typed }, param_names);
        let body = wrap_ret(erase);
        Ok(quote! {
            #preamble
            pub fn #py_name(
                stream: pyo3::PyRef<'_, ::wingfoil_next_python::Stream>,
                #(#param_decls),*
            ) -> #ret_ty {
                let __obj = stream.object();
                let __typed = #typed_in;
                let __out = #call #unwrap;
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
        if matches!(elems.len(), 1 | 2) {
            return Ok(elems);
        }
    }
    Err(Error::new(
        ty.span(),
        "#[pyop] supports one- or two-input ops (`type In<'a> = (&'a A,)` or \
         `(&'a A, &'a B)`); use `PyStream::wire_op1`/`wire_op2` for other shapes",
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

    let (param, cfg_value) = if is_unit(&cfg_ty) {
        (quote! {}, quote! { () })
    } else {
        let arg = args
            .arg
            .clone()
            .unwrap_or_else(|| Ident::new("cfg", name.span()));
        (quote! { , #arg: #cfg_ty }, quote! { #arg })
    };
    let state_seed = quote! { || <#state_ty as ::core::default::Default>::default() };

    let body = if elems.len() == 1 {
        let a_ty = &elems[0];
        quote! {
            #[pyo3::pyfunction]
            fn #name(
                stream: pyo3::PyRef<'_, ::wingfoil_next_python::Stream>
                #param
            ) -> ::wingfoil_next_python::Stream {
                ::wingfoil_next_python::Stream::from(
                    ::wingfoil_next_python::PyStream::wire_op1::<#a_ty, _, _, #out_ty, _, _>(
                        stream.object(),
                        #name_str,
                        <#self_ty as ::wingfoil_next_python::Op>::ACTIVATION,
                        #cfg_value,
                        #state_seed,
                        |__c, __s, __a: &#a_ty, __ctx| {
                            <#self_ty as ::wingfoil_next_python::Op>::cycle(__c, __s, (__a,), __ctx)
                        },
                    )
                )
            }
        }
    } else {
        let a_ty = &elems[0];
        let b_ty = &elems[1];
        quote! {
            #[pyo3::pyfunction]
            fn #name(
                stream: pyo3::PyRef<'_, ::wingfoil_next_python::Stream>,
                other: pyo3::PyRef<'_, ::wingfoil_next_python::Stream>
                #param
            ) -> ::wingfoil_next_python::Stream {
                ::wingfoil_next_python::Stream::from(
                    ::wingfoil_next_python::PyStream::wire_op2::<#a_ty, #b_ty, _, _, #out_ty, _, _>(
                        stream.object(),
                        other.object(),
                        #name_str,
                        <#self_ty as ::wingfoil_next_python::Op>::ACTIVATION,
                        #cfg_value,
                        #state_seed,
                        |__c, __s, __a: &#a_ty, __b: &#b_ty, __ctx| {
                            <#self_ty as ::wingfoil_next_python::Op>::cycle(
                                __c, __s, (__a, __b), __ctx,
                            )
                        },
                    )
                )
            }
        }
    };
    Ok(body)
}

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
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{
    Error, FnArg, GenericArgument, Ident, ImplItem, ItemFn, ItemImpl, Pat, PathArguments,
    ReturnType, Token, Type, parse_macro_input,
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

/// `#[pyadapter(name = ...[, source])]` — expose a user adapter method as a
/// Python callable, edge-converting at the boundary. Two shapes:
///
/// - **source** (`source` marker): `impl Trait for GraphBuilder { fn m(&self,
///   args…) -> Stream<T> }` => `module.name(graph, args…)` — runs the adapter on
///   the caller's builder and erases the `T` output.
/// - **sink / transform** (no marker): `impl Trait for Stream<T> { fn m(&self,
///   args…) -> Stream<U> }` => `module.name(stream, args…)` — extracts the input
///   to native `T`, runs the adapter, and erases the `U` output (a sink's
///   `Stream<()>` erases to Python `None`).
///
/// v1 covers single-value streams; burst (`Stream<Burst<T>>`) adapters are not
/// yet emitted.
#[proc_macro_attribute]
pub fn pyadapter(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as PyAdapterArgs);
    let imp = parse_macro_input!(item as ItemImpl);
    match expand_pyadapter(&args, &imp) {
        Ok(extra) => quote! { #imp #extra }.into(),
        Err(e) => {
            let e = e.to_compile_error();
            quote! { #imp #e }.into()
        }
    }
}

fn expand_pyadapter(args: &PyAdapterArgs, imp: &ItemImpl) -> syn::Result<TokenStream2> {
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
    let method_name = &method.sig.ident;

    // Non-receiver params: (name, type), forwarded to the method verbatim.
    let mut param_decls = Vec::new();
    let mut param_names = Vec::new();
    for arg in &method.sig.inputs {
        match arg {
            FnArg::Receiver(_) => {}
            FnArg::Typed(pt) => {
                let name = match &*pt.pat {
                    Pat::Ident(pi) => pi.ident.clone(),
                    _ => {
                        return Err(Error::new(
                            pt.pat.span(),
                            "#[pyadapter] method params must be simple identifiers",
                        ));
                    }
                };
                let ty = &pt.ty;
                param_decls.push(quote! { #name: #ty });
                param_names.push(name);
            }
        }
    }

    let out_ty = match &method.sig.output {
        ReturnType::Type(_, ty) => &**ty,
        ReturnType::Default => {
            return Err(Error::new(
                method.sig.span(),
                "#[pyadapter] adapter method must return `Stream<T>`",
            ));
        }
    };
    let out_inner = stream_inner(out_ty)?;
    let py_name = &args.name;

    if args.is_source {
        // Source: `impl Trait for GraphBuilder { fn m(&self, args…) -> Stream<T> }`
        // => `module.name(graph, args…)`: run the adapter on the builder, erase T.
        Ok(quote! {
            #[pyo3::pyfunction]
            fn #py_name(
                graph: pyo3::PyRef<'_, ::wingfoil_next_python::Graph>,
                #(#param_decls),*
            ) -> ::wingfoil_next_python::Stream {
                let __obj = graph.object();
                let __typed = __obj.builder().#method_name(#(#param_names),*);
                ::wingfoil_next_python::Stream::from(__obj.erase_source::<#out_inner>(__typed))
            }
        })
    } else {
        // Sink / transform: `impl Trait for Stream<T> { fn m(&self, args…) ->
        // Stream<U> }` => `module.name(stream, args…)`: extract the input to
        // native `T`, run the adapter, erase the `U` output (a sink's `Stream<()>`
        // erases to Python `None`).
        let in_inner = stream_inner(&imp.self_ty)?;
        Ok(quote! {
            #[pyo3::pyfunction]
            fn #py_name(
                stream: pyo3::PyRef<'_, ::wingfoil_next_python::Stream>,
                #(#param_decls),*
            ) -> ::wingfoil_next_python::Stream {
                let __obj = stream.object();
                let __typed = __obj.typed_input::<#in_inner>();
                let __out = __typed.#method_name(#(#param_names),*);
                ::wingfoil_next_python::Stream::from(__obj.erased_output::<#out_inner>(__out))
            }
        })
    }
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

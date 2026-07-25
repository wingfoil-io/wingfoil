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
//! **Scope (v1):** stateless (`State = ()`), single-input (`In<'a> = (&'a A,)`),
//! concrete (non-generic) ops, with `Cfg = ()` or a single `FromPyObject` type.
//! Stateful / multi-input ops use `PyStream::wire_op1` directly.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{Error, Ident, ImplItem, ItemImpl, Token, Type, parse_macro_input};

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

/// The `T` of a single-element reference tuple `(&'a T,)`.
fn single_ref_tuple_elem(ty: &Type) -> syn::Result<Type> {
    if let Type::Tuple(t) = ty
        && t.elems.len() == 1
        && let Type::Reference(r) = &t.elems[0]
    {
        return Ok((*r.elem).clone());
    }
    Err(Error::new(
        ty.span(),
        "#[pyop] supports single-input ops (`type In<'a> = (&'a T,)`); use \
         `PyStream::wire_op1` for other shapes",
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

    if !is_unit(&state_ty) {
        return Err(Error::new(
            state_ty.span(),
            "#[pyop] supports stateless ops (`type State = ()`); use \
             `PyStream::wire_op1` directly for stateful ops",
        ));
    }
    let a_ty = single_ref_tuple_elem(&in_ty)?;

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

    Ok(quote! {
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
                    || (),
                    |__c, __s, __a: &#a_ty, __ctx| {
                        <#self_ty as ::wingfoil_next_python::Op>::cycle(__c, __s, (__a,), __ctx)
                    },
                )
            )
        }
    })
}

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, parse_macro_input};

/// Derives `StreamPeekRef<T>` for a struct with exactly one field tagged `#[output]`.
///
/// The generated impl delegates `peek_ref()` to a reference of that field.
///
/// # Example
///
/// ```rust,ignore
/// #[derive(StreamPeekRef)]
/// struct MyNode<T: Element> {
///     upstream: Rc<dyn Stream<T>>,
///     #[output]
///     value: T,
/// }
/// // Generates:
/// // impl<T: Element> StreamPeekRef<T> for MyNode<T> {
/// //     fn peek_ref(&self) -> &T { &self.value }
/// // }
/// ```
#[proc_macro_derive(StreamPeekRef, attributes(output))]
pub fn derive_stream_peek_ref(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(f) => &f.named,
            _ => {
                return syn::Error::new_spanned(
                    name,
                    "#[derive(StreamPeekRef)] requires named struct fields",
                )
                .to_compile_error()
                .into();
            }
        },
        _ => {
            return syn::Error::new_spanned(
                name,
                "#[derive(StreamPeekRef)] can only be applied to structs",
            )
            .to_compile_error()
            .into();
        }
    };

    let output_fields: Vec<_> = fields
        .iter()
        .filter(|f| f.attrs.iter().any(|a| a.path().is_ident("output")))
        .collect();

    let output_field = match output_fields.len() {
        0 => {
            return syn::Error::new_spanned(
                name,
                "#[derive(StreamPeekRef)] requires exactly one field tagged #[output]",
            )
            .to_compile_error()
            .into();
        }
        1 => output_fields[0],
        _ => {
            return syn::Error::new_spanned(
                name,
                "#[derive(StreamPeekRef)] found multiple #[output] fields; exactly one is required",
            )
            .to_compile_error()
            .into();
        }
    };

    let field_name = output_field.ident.as_ref().expect("named field");
    let field_type = &output_field.ty;

    // Add `Self: MutableNode` to the where clause so the impl is only valid when the
    // MutableNode supertrait (with its potentially-stricter bounds) is also satisfied.
    // This lets the derive work for structs whose MutableNode impl has extra bounds
    // (e.g. `T: ToPrimitive`, `IN1: 'static`) that aren't on the struct definition.
    let where_tokens = match where_clause {
        Some(wc) => {
            let preds = wc.predicates.iter();
            quote! { where #(#preds,)* #name #ty_generics: MutableNode }
        }
        None => quote! { where #name #ty_generics: MutableNode },
    };

    quote! {
        impl #impl_generics StreamPeekRef<#field_type> for #name #ty_generics #where_tokens {
            fn peek_ref(&self) -> &#field_type {
                &self.#field_name
            }
        }
    }
    .into()
}

/// Derives `WiringPoint` for a struct.
///
/// Fields tagged `#[active]` are collected as active upstreams (trigger execution).
/// Fields tagged `#[passive]` are collected as passive upstreams (read but don't trigger).
/// Each upstream field must implement `AsUpstreamNodes` (implemented for `Rc<dyn Node>`,
/// `Rc<dyn Stream<T>>`, and `Vec<U: AsUpstreamNodes>`).
///
/// If no fields are tagged, the generated impl returns `UpStreams::none()` (source node).
///
/// # Example
///
/// ```rust,ignore
/// #[derive(WiringPoint)]
/// struct MapStream<IN, OUT: Element> {
///     #[active]
///     upstream: Rc<dyn Stream<IN>>,
///     #[output]
///     value: OUT,
///     func: Box<dyn Fn(IN) -> OUT>,
/// }
/// // Generates:
/// // impl<IN, OUT: Element> WiringPoint for MapStream<IN, OUT>
/// // where MapStream<IN, OUT>: MutableNode {
/// //     fn upstreams(&self) -> UpStreams {
/// //         let mut active = Vec::new();
/// //         let mut passive = Vec::new();
/// //         active.extend(AsUpstreamNodes::as_upstream_nodes(&self.upstream));
/// //         UpStreams::new(active, passive)
/// //     }
/// // }
/// ```
#[proc_macro_derive(WiringPoint, attributes(active, passive))]
pub fn derive_upstreams(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(f) => &f.named,
            _ => {
                return syn::Error::new_spanned(
                    name,
                    "#[derive(WiringPoint)] requires named struct fields",
                )
                .to_compile_error()
                .into();
            }
        },
        _ => {
            return syn::Error::new_spanned(
                name,
                "#[derive(WiringPoint)] can only be applied to structs",
            )
            .to_compile_error()
            .into();
        }
    };

    let active_fields: Vec<_> = fields
        .iter()
        .filter(|f| f.attrs.iter().any(|a| a.path().is_ident("active")))
        .collect();

    let passive_fields: Vec<_> = fields
        .iter()
        .filter(|f| f.attrs.iter().any(|a| a.path().is_ident("passive")))
        .collect();

    // Add `where Self: MutableNode` so the impl is only valid when MutableNode is also
    // satisfied.  This lets the derive work for structs whose MutableNode impl has extra
    // bounds (e.g. `IN1: 'static`) that aren't on the struct definition.
    let where_tokens = match where_clause {
        Some(wc) => {
            let preds = wc.predicates.iter();
            quote! { where #(#preds,)* #name #ty_generics: MutableNode }
        }
        None => quote! { where #name #ty_generics: MutableNode },
    };

    if active_fields.is_empty() && passive_fields.is_empty() {
        // Source node: no upstreams, emit UpStreams::none()
        return quote! {
            impl #impl_generics WiringPoint for #name #ty_generics #where_tokens {
                fn upstreams(&self) -> UpStreams {
                    UpStreams::none()
                }
            }
        }
        .into();
    }

    let active_extend = active_fields.iter().map(|f| {
        let field_name = f.ident.as_ref().expect("named field");
        quote! {
            active.extend(AsUpstreamNodes::as_upstream_nodes(&self.#field_name));
        }
    });

    let passive_extend = passive_fields.iter().map(|f| {
        let field_name = f.ident.as_ref().expect("named field");
        quote! {
            passive.extend(AsUpstreamNodes::as_upstream_nodes(&self.#field_name));
        }
    });

    quote! {
        impl #impl_generics WiringPoint for #name #ty_generics #where_tokens {
            fn upstreams(&self) -> UpStreams {
                let mut active: ::std::vec::Vec<::std::rc::Rc<dyn Node>> = ::std::vec::Vec::new();
                let mut passive: ::std::vec::Vec<::std::rc::Rc<dyn Node>> = ::std::vec::Vec::new();
                #(#active_extend)*
                #(#passive_extend)*
                UpStreams::new(active, passive)
            }
        }
    }
    .into()
}

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{
    Attribute, Ident, ImplItem, ImplItemFn, ItemImpl, LitStr, Token, Type, Visibility, braced,
    bracketed,
    parse::{Parse, ParseStream},
    parse_macro_input,
    punctuated::Punctuated,
};

struct NodeArgs {
    active: Vec<Ident>,
    passive: Vec<Ident>,
    output: Option<(Ident, Type)>,
}

impl Parse for NodeArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut active: Option<Vec<Ident>> = None;
        let mut passive: Option<Vec<Ident>> = None;
        let mut output: Option<(Ident, Type)> = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;

            match key.to_string().as_str() {
                "active" => {
                    if active.is_some() {
                        return Err(syn::Error::new(key.span(), "duplicate key `active`"));
                    }
                    let content;
                    bracketed!(content in input);
                    let list = Punctuated::<Ident, Token![,]>::parse_terminated(&content)?;
                    active = Some(list.into_iter().collect());
                }
                "passive" => {
                    if passive.is_some() {
                        return Err(syn::Error::new(key.span(), "duplicate key `passive`"));
                    }
                    let content;
                    bracketed!(content in input);
                    let list = Punctuated::<Ident, Token![,]>::parse_terminated(&content)?;
                    passive = Some(list.into_iter().collect());
                }
                "output" => {
                    if output.is_some() {
                        return Err(syn::Error::new(key.span(), "duplicate key `output`"));
                    }
                    let field: Ident = input.parse()?;
                    input.parse::<Token![:]>()?;
                    let ty: Type = input.parse()?;
                    output = Some((field, ty));
                }
                _ => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown key `{key}`; expected `active`, `passive`, or `output`"),
                    ));
                }
            }

            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        Ok(NodeArgs {
            active: active.unwrap_or_default(),
            passive: passive.unwrap_or_default(),
            output,
        })
    }
}

/// Attribute macro that reduces boilerplate in [`MutableNode`] implementations.
///
/// Place `#[node(...)]` on an `impl MutableNode for MyType` block. It can:
///
/// - Inject `fn upstreams()` from `active = [field1, field2]` and/or `passive = [field3]`
/// - Emit a separate `impl StreamPeekRef<T>` from `output = field_name: FieldType`
///
/// Fields listed as `active` or `passive` must implement `AsUpstreamNodes`
/// (`Rc<dyn Node>`, `Rc<dyn Stream<T>>`, or `Vec` of either).
///
/// If neither `active` nor `passive` is specified, no `upstreams()` is injected —
/// the default `UpStreams::none()` from [`MutableNode`] is used (source node), or you
/// can write `upstreams()` manually in the impl block for complex cases (e.g. `Dep<T>`).
///
/// # Examples
///
/// ```rust,ignore
/// // Transform node: active upstream, stream output
/// #[node(active = [upstream], output = value: OUT)]
/// impl<IN, OUT: Element> MutableNode for MapStream<IN, OUT> {
///     fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
///         self.value = (self.func)(self.upstream.peek_value());
///         Ok(true)
///     }
/// }
///
/// // Sink node: active upstream, no output
/// #[node(active = [upstream])]
/// impl<IN> MutableNode for ConsumerNode<IN> {
///     fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> { ... }
/// }
///
/// // Source node with output (upstreams default to none)
/// #[node(output = value: T)]
/// impl<T: Element> MutableNode for ConstantStream<T> {
///     fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> { Ok(true) }
/// }
///
/// // Mixed: passive + active upstreams, output
/// #[node(passive = [upstream], active = [trigger], output = value: T)]
/// impl<T: Element> MutableNode for SampleStream<T> { ... }
///
/// // Complex upstreams (Dep<T> etc.) — write upstreams() manually, use #[node] for output only
/// #[node(output = value: OUT)]
/// impl<IN1: 'static, IN2: 'static, OUT: Element> MutableNode for BiMapStream<IN1, IN2, OUT> {
///     fn cycle(&mut self, ...) -> anyhow::Result<bool> { ... }
///     fn upstreams(&self) -> UpStreams { /* Dep<T> logic */ }
/// }
/// ```
#[proc_macro_attribute]
pub fn node(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as NodeArgs);
    let mut impl_block = parse_macro_input!(item as ItemImpl);

    let self_ty = impl_block.self_ty.clone();
    let (impl_generics, _, where_clause) = impl_block.generics.split_for_impl();

    // Inject fn upstreams() if active/passive fields are specified.
    if !args.active.is_empty() || !args.passive.is_empty() {
        let active_fields = &args.active;
        let passive_fields = &args.passive;

        // All wingfoil paths are fully qualified so the generated code is
        // hygienic against user-defined traits/types of the same name.
        // Inside the wingfoil crate itself, `::wingfoil` resolves via
        // `extern crate self as wingfoil;` in lib.rs.
        let upstreams_fn: ImplItemFn = syn::parse_quote! {
            fn upstreams(&self) -> ::wingfoil::UpStreams {
                let mut active: ::std::vec::Vec<::std::rc::Rc<dyn ::wingfoil::Node>> = ::std::vec::Vec::new();
                let mut passive: ::std::vec::Vec<::std::rc::Rc<dyn ::wingfoil::Node>> = ::std::vec::Vec::new();
                #(active.extend(::wingfoil::AsUpstreamNodes::as_upstream_nodes(&self.#active_fields));)*
                #(passive.extend(::wingfoil::AsUpstreamNodes::as_upstream_nodes(&self.#passive_fields));)*
                ::wingfoil::UpStreams::new(active, passive)
            }
        };
        impl_block.items.push(ImplItem::Fn(upstreams_fn));
    }

    // Emit a StreamPeekRef impl if output is specified.
    let peek_ref_impl = args.output.map(|(field, ty)| {
        quote! {
            impl #impl_generics ::wingfoil::StreamPeekRef<#ty> for #self_ty #where_clause {
                fn peek_ref(&self) -> &#ty {
                    &self.#field
                }
            }
        }
    });

    quote! {
        #impl_block
        #peek_ref_impl
    }
    .into()
}

// =============================================================================
// latency_stages! — declare a fixed-size, named-field latency record.
// =============================================================================

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
        // invariants. Only emitted when the `iceoryx2-beta` feature is on
        // in the consuming crate.
        #[cfg(feature = "iceoryx2-beta")]
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

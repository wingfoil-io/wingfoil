use proc_macro::TokenStream;
use quote::quote;
use syn::{
    Ident, ImplItem, ImplItemFn, ItemImpl, Token, Type, bracketed,
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

        let upstreams_fn: ImplItemFn = syn::parse_quote! {
            fn upstreams(&self) -> UpStreams {
                let mut active: ::std::vec::Vec<::std::rc::Rc<dyn Node>> = ::std::vec::Vec::new();
                let mut passive: ::std::vec::Vec<::std::rc::Rc<dyn Node>> = ::std::vec::Vec::new();
                #(active.extend(AsUpstreamNodes::as_upstream_nodes(&self.#active_fields));)*
                #(passive.extend(AsUpstreamNodes::as_upstream_nodes(&self.#passive_fields));)*
                UpStreams::new(active, passive)
            }
        };
        impl_block.items.push(ImplItem::Fn(upstreams_fn));
    }

    // Emit a StreamPeekRef impl if output is specified.
    let peek_ref_impl = args.output.map(|(field, ty)| {
        quote! {
            impl #impl_generics StreamPeekRef<#ty> for #self_ty #where_clause {
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

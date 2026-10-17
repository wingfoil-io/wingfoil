//! Composite faces: a fluent method that wires **several** nodes — one op and
//! the projections that split its output, say — made visible to `nitro!`.
//!
//! `nitro!` sees tokens, never function bodies, so a face like
//! `parents.algo(&landed, &reports, &clock)` (an op plus two filters,
//! returning a pair) is opaque to it: there is no single forwarder family to
//! dispatch. A composite closes that gap by handing the macro the face's
//! *body*, and the macro inlines it as ordinary nodes — so a compiled graph
//! that calls a composite is exactly the graph its body wires, op for op.
//!
//! # The callback
//!
//! A proc macro cannot look a body up, so the body comes to it. The face's
//! author writes a small `macro_rules!` **shell** holding the definition:
//!
//! ```ignore
//! #[doc(hidden)]
//! #[macro_export]
//! macro_rules! __wf_composite_algo {
//!     ($($cb:tt)*) => {
//!         $crate::composite! { [$($cb)*]
//!             pub fn algo<I: Instrument + 'static>(
//!                 parents: &Stream<Burst<Parent<I>>>,
//!                 landed: &Stream<Burst<Request<I>>>,
//!                 ..
//!             ) -> (Stream<..>, Stream<..>) {
//!                 let node = landed.algo_op(reports, parents, clock);
//!                 ..
//!                 (desired, progress)
//!             }
//!         }
//!     };
//! }
//! #[doc(hidden)]
//! pub use crate::__wf_composite_algo;
//! __wf_composite_algo!(define);
//! ```
//!
//! `__wf_composite_algo!(define)` emits the definition as an ordinary
//! function — the face's fluent implementation, which the extension trait's
//! method calls. When a `nitro!` body binds a tuple to a chain
//! (`let (d, p) = parents.algo(..);`) and has no definition for `algo` yet, it
//! expands to a call of that shell carrying its own input, and the shell
//! hands the definition back through [`composite`](crate::composite) to a
//! second `nitro!` pass that now has it. One round trip per distinct
//! composite; a composite whose body calls another resolves the inner one the
//! same way.
//!
//! The shell is hand-written because it has to be: a `#[macro_export]` macro
//! *generated* by a proc macro cannot be re-exported by path (rust-lang/rust
//! #52234, deny-by-default and slated to become a hard error), and the
//! re-export is what lets a glob import of the face's module — the import
//! that already brings its op's forwarders — bring the shell too.
//!
//! # Inlining
//!
//! The body is written in `nitro!`'s own grammar, and is valid fluent Rust
//! besides (it *is* the `define`d function). Inlining renames it into the
//! call site:
//!
//! - a **stream parameter** becomes the stream the call passed (the receiver
//!   for the first; `&name` or a bare input name for the rest);
//! - a **builder parameter** (`g: &GraphBuilder`, first only, for a face on
//!   the builder) becomes the graph's builder;
//! - a **plain parameter** is bound once to a fresh local evaluated where the
//!   call stands — as the fluent call evaluates its arguments once — except a
//!   literal closure, which is substituted where the body uses it so the op
//!   still sees a literal closure (the inference deferral `nitro!` relies on);
//! - every name the body binds becomes a fresh `wf_anon_c<k>_<name>`, except
//!   the ones its tail returns, which take the call site's pattern names.
//!
//! Renaming is on the syntax tree, so method names, fields and multi-segment
//! paths are never touched; tokens inside a macro call in the body are not
//! visited either, so a body must not name a parameter or binding inside one.

use std::collections::{HashMap, HashSet};

use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::visit_mut::{self, VisitMut};
use syn::{Expr, FnArg, Ident, ItemFn, Pat, ReturnType, Stmt, Token, Type};

use crate::pattern_idents;

/// How a composite parameter binds at a call site.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ParamKind {
    /// `&Stream<T>` — an edge.
    Stream,
    /// `&GraphBuilder` — the builder, for a face rooted at it.
    Builder,
    /// Anything else — a value the body's ops take as config.
    Plain,
}

/// A composite's definition, checked: the shape `nitro!` inlines.
pub(crate) struct CompositeDef {
    /// The definition, verbatim — re-emitted by `define`, and handed on to
    /// later `nitro!` passes.
    pub(crate) item: ItemFn,
    params: Vec<(Ident, ParamKind)>,
    /// The body's statements, tail excluded.
    body: Vec<Stmt>,
    /// The bindings the tail returns, in order.
    tail: Vec<Ident>,
    /// Every name the body's `let`s bind.
    bound: Vec<Ident>,
    /// The plain parameters a literal-closure argument may be substituted
    /// for: each use is the only config argument of its op call.
    inline_closures: HashSet<String>,
}

fn param_kind(ty: &Type) -> ParamKind {
    match referenced_name(ty).as_deref() {
        Some("Stream") => ParamKind::Stream,
        Some("GraphBuilder") => ParamKind::Builder,
        _ => ParamKind::Plain,
    }
}

/// `name: Type` → the name and how it binds at a call site.
fn typed_param(arg: &FnArg) -> syn::Result<(Ident, ParamKind)> {
    let FnArg::Typed(arg) = arg else {
        return Err(syn::Error::new(
            arg.span(),
            "a face is a free function: its receiver is its first parameter, not `self`",
        ));
    };
    let Pat::Ident(pat) = &*arg.pat else {
        return Err(syn::Error::new(
            arg.pat.span(),
            "a face's parameters must be plain names",
        ));
    };
    Ok((pat.ident.clone(), param_kind(&arg.ty)))
}

/// Every name any pattern in `stmts` binds — `let`s and closure parameters
/// alike, at any depth.
fn all_bindings(stmts: &[Stmt]) -> Vec<Ident> {
    struct Collect(Vec<Ident>);
    impl<'ast> syn::visit::Visit<'ast> for Collect {
        fn visit_pat_ident(&mut self, pat: &'ast syn::PatIdent) {
            self.0.push(pat.ident.clone());
            syn::visit::visit_pat_ident(self, pat);
        }
    }
    let mut c = Collect(Vec::new());
    for stmt in stmts {
        syn::visit::Visit::visit_stmt(&mut c, stmt);
    }
    c.0
}

/// The plain parameters whose every use in `stmts` is the only config
/// argument of a method call — the position where `nitro!` takes a literal
/// closure as the op's whole `Cfg`. A literal closure is substituted only for
/// those; one passed to any other parameter is bound to a local like any
/// plain argument, so an op with a tuple `Cfg` still gets the tuple.
fn sole_config_params(
    stmts: &[Stmt],
    plain: &[&Ident],
    edges: &HashSet<String>,
) -> HashSet<String> {
    struct Uses<'a> {
        plain: &'a [&'a Ident],
        edges: &'a HashSet<String>,
        sole: HashMap<String, bool>,
    }
    fn named(e: &Expr) -> Option<String> {
        match e {
            Expr::Path(p) if p.qself.is_none() => p.path.get_ident().map(Ident::to_string),
            Expr::Reference(r) => named(&r.expr),
            _ => None,
        }
    }
    impl<'ast> syn::visit::Visit<'ast> for Uses<'_> {
        fn visit_expr_method_call(&mut self, mc: &'ast syn::ExprMethodCall) {
            let config = mc
                .args
                .iter()
                .filter(|a| !named(a).is_some_and(|n| self.edges.contains(&n)))
                .count();
            for arg in &mc.args {
                if let Expr::Path(_) = arg
                    && let Some(n) = named(arg)
                    && self.plain.iter().any(|p| *p == &n)
                {
                    let ok = self.sole.entry(n).or_insert(true);
                    *ok &= config == 1;
                } else {
                    syn::visit::visit_expr(self, arg);
                }
            }
            syn::visit::visit_expr(self, &mc.receiver);
        }
        fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
            // A use anywhere but a method-call argument, which the arm above
            // has already taken.
            if let Some(id) = p.path.get_ident()
                && self.plain.contains(&id)
            {
                self.sole.insert(id.to_string(), false);
            }
        }
    }
    let mut uses = Uses {
        plain,
        edges,
        sole: HashMap::new(),
    };
    for stmt in stmts {
        syn::visit::Visit::visit_stmt(&mut uses, stmt);
    }
    uses.sole
        .into_iter()
        .filter_map(|(n, ok)| ok.then_some(n))
        .collect()
}

/// The last path segment of `ty` behind a reference, if it is one.
fn referenced_name(ty: &Type) -> Option<String> {
    let Type::Reference(r) = ty else {
        return None;
    };
    let Type::Path(p) = &*r.elem else {
        return None;
    };
    p.path.segments.last().map(|s| s.ident.to_string())
}

/// How many streams a composite's return type declares.
fn returned_streams(ret: &ReturnType) -> Option<usize> {
    let is_stream = |ty: &Type| matches!(ty, Type::Path(p) if p.path.segments.last().is_some_and(|s| s.ident == "Stream"));
    match ret {
        ReturnType::Type(_, ty) => match &**ty {
            Type::Tuple(t) if !t.elems.is_empty() && t.elems.iter().all(is_stream) => {
                Some(t.elems.len())
            }
            other if is_stream(other) => Some(1),
            _ => None,
        },
        ReturnType::Default => None,
    }
}

impl CompositeDef {
    pub(crate) fn name(&self) -> &Ident {
        &self.item.sig.ident
    }

    pub(crate) fn new(item: ItemFn) -> syn::Result<Self> {
        let sig = &item.sig;
        let mut params = Vec::with_capacity(sig.inputs.len());
        for (i, arg) in sig.inputs.iter().enumerate() {
            let (ident, kind) = typed_param(arg)?;
            if i == 0 && kind == ParamKind::Plain {
                return Err(syn::Error::new(
                    arg.span(),
                    "a composite's first parameter is its receiver: `&Stream<T>`, or \
                     `&GraphBuilder` for a face on the builder",
                ));
            }
            if i > 0 && kind == ParamKind::Builder {
                return Err(syn::Error::new(
                    arg.span(),
                    "only a composite's first parameter may be the builder",
                ));
            }
            params.push((ident, kind));
        }
        if params.is_empty() {
            return Err(syn::Error::new(
                sig.span(),
                "a composite takes its receiver as its first parameter",
            ));
        }
        let Some(declared) = returned_streams(&sig.output) else {
            return Err(syn::Error::new(
                sig.output.span(),
                "a composite returns `Stream<T>` or a tuple of `Stream<T>`s",
            ));
        };

        let mut body = item.block.stmts.clone();
        let tail = match body.pop() {
            Some(Stmt::Expr(expr, None)) => expr,
            _ => {
                return Err(syn::Error::new(
                    item.block.span(),
                    "a composite's body ends with the binding(s) it returns, e.g. `(a, b)`",
                ));
            }
        };
        let tail_ident = |e: &Expr| match e {
            Expr::Path(p) => p.path.get_ident().cloned(),
            _ => None,
        };
        let tail: Vec<Ident> = match &tail {
            Expr::Tuple(t) => t.elems.iter().map(tail_ident).collect::<Option<_>>(),
            other => tail_ident(other).map(|i| vec![i]),
        }
        .ok_or_else(|| {
            syn::Error::new(
                tail.span(),
                "a composite's tail must name bindings from its body: `out` or `(a, b)`",
            )
        })?;
        if tail.len() != declared {
            return Err(syn::Error::new(
                tail.first().map_or(sig.span(), Ident::span),
                format!(
                    "the return type declares {declared} stream(s) but the tail names {}",
                    tail.len()
                ),
            ));
        }

        let mut bound = Vec::new();
        for stmt in &body {
            if let Stmt::Local(local) = stmt {
                pattern_idents(&local.pat, &mut bound);
            }
        }
        // Renaming is one map for the whole body, so a name may mean one
        // thing in it: a binding — a `let`, or a closure's parameter — that
        // reuses a parameter's name would take the parameter's renaming.
        let param_names: HashSet<String> = params.iter().map(|(p, _)| p.to_string()).collect();
        if let Some(shadow) = all_bindings(&body)
            .into_iter()
            .find(|b| param_names.contains(&b.to_string()))
        {
            return Err(syn::Error::new(
                shadow.span(),
                format!(
                    "`{shadow}` rebinds a parameter of this composite; give it its own name — \
                     inlining renames each name once, for the whole body"
                ),
            ));
        }
        let bound_names: HashSet<String> = bound.iter().map(Ident::to_string).collect();
        let mut seen = HashSet::new();
        for t in &tail {
            if !bound_names.contains(&t.to_string()) {
                return Err(syn::Error::new(
                    t.span(),
                    format!(
                        "`{t}` is not bound in this composite's body — a composite returns \
                         streams its body wires, never a parameter"
                    ),
                ));
            }
            if !seen.insert(t.to_string()) {
                return Err(syn::Error::new(
                    t.span(),
                    format!("`{t}` is returned twice"),
                ));
            }
        }
        let mut edges: HashSet<String> = params
            .iter()
            .filter(|(_, k)| *k != ParamKind::Plain)
            .map(|(p, _)| p.to_string())
            .collect();
        edges.extend(bound_names);
        let plain: Vec<&Ident> = params
            .iter()
            .filter(|(_, k)| *k == ParamKind::Plain)
            .map(|(p, _)| p)
            .collect();
        let inline_closures = sole_config_params(&body, &plain, &edges);
        Ok(Self {
            item,
            params,
            body,
            tail,
            bound,
            inline_closures,
        })
    }

    /// The names a call site may use for each stream argument, given the
    /// parameter kinds — what the prepass needs to keep tracking streams
    /// through a composite it has not inlined yet.
    pub(crate) fn stream_params(&self) -> impl Iterator<Item = &Ident> {
        self.params
            .iter()
            .filter(|(_, k)| *k != ParamKind::Plain)
            .map(|(id, _)| id)
    }

    /// The body, for the prepass: which composites it calls in turn.
    pub(crate) fn body(&self) -> &[Stmt] {
        &self.body
    }

    /// Inline one call: the statements `nitro!` walks in its place.
    ///
    /// `outs` are the call site's pattern names, `None` for a `_`; `k` makes
    /// this call's fresh names unique; `is_stream` says whether a name is a
    /// stream bound at the call site.
    pub(crate) fn inline(
        &self,
        receiver: &Ident,
        builder: &Ident,
        args: &[&Expr],
        outs: &[Option<Ident>],
        k: usize,
        is_stream: impl Fn(&Ident) -> bool,
    ) -> syn::Result<Vec<Stmt>> {
        let name = self.name();
        if args.len() + 1 != self.params.len() {
            return Err(syn::Error::new(
                receiver.span(),
                format!(
                    "composite `{name}` takes {} argument(s) after its receiver; this call \
                     passes {}",
                    self.params.len() - 1,
                    args.len()
                ),
            ));
        }
        if outs.len() != self.tail.len() {
            return Err(syn::Error::new(
                receiver.span(),
                format!(
                    "composite `{name}` returns {} stream(s); this pattern binds {}",
                    self.tail.len(),
                    outs.len()
                ),
            ));
        }

        let mut rename: HashMap<String, Ident> = HashMap::new();
        let mut closures: HashMap<String, Expr> = HashMap::new();
        let mut prelude: Vec<Stmt> = Vec::new();
        let fresh = |base: &Ident| Ident::new(&format!("wf_anon_c{k}_{base}"), Span::call_site());

        let (first, first_kind) = &self.params[0];
        match first_kind {
            ParamKind::Builder if receiver != builder => {
                return Err(syn::Error::new(
                    receiver.span(),
                    format!(
                        "composite `{name}` is a face on the builder: call it as `{builder}.{name}(..)`"
                    ),
                ));
            }
            ParamKind::Stream if receiver == builder || !is_stream(receiver) => {
                return Err(syn::Error::new(
                    receiver.span(),
                    format!("composite `{name}` is a face on a stream: its receiver must be one"),
                ));
            }
            _ => {}
        }
        rename.insert(first.to_string(), receiver.clone());

        for ((param, kind), arg) in self.params[1..].iter().zip(args) {
            match kind {
                ParamKind::Stream | ParamKind::Builder => {
                    let target = stream_arg(arg).filter(|id| match kind {
                        ParamKind::Builder => id == builder,
                        _ => is_stream(id),
                    });
                    let Some(target) = target else {
                        let want = if *kind == ParamKind::Builder {
                            format!("the builder `{builder}`")
                        } else {
                            "`&name` naming a stream bound in this graph".to_string()
                        };
                        return Err(syn::Error::new(
                            arg.span(),
                            format!("composite `{name}`'s parameter `{param}` takes {want}"),
                        ));
                    };
                    rename.insert(param.to_string(), target);
                }
                ParamKind::Plain
                    if matches!(arg, Expr::Closure(_))
                        && self.inline_closures.contains(&param.to_string()) =>
                {
                    closures.insert(param.to_string(), (*arg).clone());
                }
                ParamKind::Plain => {
                    let local = fresh(param);
                    prelude.push(syn::parse_quote! { let #local = #arg; });
                    rename.insert(param.to_string(), local);
                }
            }
        }

        // Every name the body binds, renamed; the returned ones to the call
        // site's pattern, a `_` keeping its fresh name.
        let returned: HashMap<String, &Option<Ident>> =
            self.tail.iter().map(Ident::to_string).zip(outs).collect();
        for b in &self.bound {
            let to = match returned.get(&b.to_string()) {
                Some(Some(out)) => out.clone(),
                _ => fresh(b),
            };
            rename.insert(b.to_string(), to);
        }

        let mut renamer = Renamer {
            rename: &rename,
            closures: &closures,
        };
        let mut stmts = prelude;
        for stmt in &self.body {
            let mut stmt = stmt.clone();
            renamer.visit_stmt_mut(&mut stmt);
            stmts.push(stmt);
        }
        Ok(stmts)
    }
}

/// `&name` or a bare `name`: the stream (or builder) an argument names.
fn stream_arg(arg: &Expr) -> Option<Ident> {
    let path = match arg {
        Expr::Reference(r) => &*r.expr,
        other => other,
    };
    match path {
        Expr::Path(p) if p.qself.is_none() => p.path.get_ident().cloned(),
        _ => None,
    }
}

/// Renames single-segment value paths and pattern bindings; substitutes a
/// literal-closure argument where its parameter is used.
struct Renamer<'a> {
    rename: &'a HashMap<String, Ident>,
    closures: &'a HashMap<String, Expr>,
}

impl VisitMut for Renamer<'_> {
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        if let Expr::Path(p) = expr
            && p.qself.is_none()
            && let Some(id) = p.path.get_ident()
        {
            let key = id.to_string();
            if let Some(closure) = self.closures.get(&key) {
                *expr = closure.clone();
            } else if let Some(to) = self.rename.get(&key) {
                p.path.segments[0].ident = to.clone();
            }
            return;
        }
        visit_mut::visit_expr_mut(self, expr);
    }

    fn visit_pat_ident_mut(&mut self, pat: &mut syn::PatIdent) {
        if let Some(to) = self.rename.get(&pat.ident.to_string()) {
            pat.ident = to.clone();
        }
        visit_mut::visit_pat_ident_mut(self, pat);
    }

    // `Foo { name }` is shorthand for `Foo { name: name }`; renaming the value
    // must spell the field out, or the field would be renamed with it.
    fn visit_field_value_mut(&mut self, field: &mut syn::FieldValue) {
        if field.colon_token.is_none() {
            field.colon_token = Some(Token![:](field.member.span()));
        }
        visit_mut::visit_field_value_mut(self, field);
    }

    // Likewise a struct pattern's shorthand field binding.
    fn visit_field_pat_mut(&mut self, field: &mut syn::FieldPat) {
        if field.colon_token.is_none() {
            field.colon_token = Some(Token![:](field.member.span()));
        }
        visit_mut::visit_field_pat_mut(self, field);
    }
}

/// `composite!`'s input: `[<callback>] <definition>`.
pub(crate) struct CompositeInput {
    callback: TokenStream2,
    def: CompositeDef,
}

impl Parse for CompositeInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let content;
        syn::bracketed!(content in input);
        let callback: TokenStream2 = content.parse()?;
        let item: ItemFn = input.parse()?;
        if !input.is_empty() {
            return Err(input.error("composite! takes exactly one function definition"));
        }
        Ok(Self {
            callback,
            def: CompositeDef::new(item)?,
        })
    }
}

/// What `composite!` expands to: the definition itself under `define`, or
/// the next `nitro!` pass with the definition added under `@nitro`.
pub(crate) fn expand(input: CompositeInput) -> syn::Result<TokenStream2> {
    let item = &input.def.item;
    let mut cb = input.callback.clone().into_iter();
    match (cb.next(), cb.next(), cb.next()) {
        (Some(proc_macro2::TokenTree::Ident(id)), None, None) if id == "define" => {
            Ok(quote! { #item })
        }
        (
            Some(proc_macro2::TokenTree::Punct(at)),
            Some(proc_macro2::TokenTree::Ident(id)),
            Some(proc_macro2::TokenTree::Ident(asked)),
        ) if at.as_char() == '@' && id == "nitro" => {
            // The shell must hand back the face it is named for: `nitro!`
            // would otherwise ask for `asked` again on every pass, until the
            // recursion limit says nothing useful.
            let name = input.def.name();
            if *name != asked {
                return Err(syn::Error::new(
                    name.span(),
                    format!(
                        "this definition is `{name}`, but its shell `{}!` was asked for \
                         `{asked}` — a shell holds the face it is named for",
                        shell(&asked)
                    ),
                ));
            }
            let Some(proc_macro2::TokenTree::Group(known)) = cb.next() else {
                return Err(syn::Error::new(
                    input.callback.span(),
                    "malformed `@nitro` callback",
                ));
            };
            let known = known.stream();
            let rest: TokenStream2 = cb.collect();
            Ok(quote! { ::wingfoil::nitro! { @composites [ #known #item ] #rest } })
        }
        _ => Err(syn::Error::new(
            input.callback.span(),
            "composite!'s callback is `define` (emit the function) or what `nitro!` passes — \
             invoke the composite's shell as `__wf_composite_<name>!(define);`",
        )),
    }
}

/// The macro ident a composite's shell is named by.
pub(crate) fn shell(method: &Ident) -> Ident {
    format_ident!("__wf_composite_{}", method, span = method.span())
}

/// `let (a, b) = <chain>` → the pattern's names (`None` for `_`), if the
/// pattern is a tuple — the tell that the chain's call is a composite.
pub(crate) fn tuple_outs(pat: &Pat) -> Option<syn::Result<Vec<Option<Ident>>>> {
    let Pat::Tuple(t) = pat else {
        return None;
    };
    Some(
        t.elems
            .iter()
            .map(|e| match e {
                Pat::Ident(p) if p.subpat.is_none() && p.by_ref.is_none() => {
                    Ok(Some(p.ident.clone()))
                }
                Pat::Wild(_) => Ok(None),
                other => Err(syn::Error::new(
                    other.span(),
                    "a composite's outputs bind to plain names (or `_`)",
                )),
            })
            .collect(),
    )
}

/// The one call of a composite chain: `receiver.method(args)`. A composite
/// returns a tuple, so nothing can follow it in the chain, and it has no
/// type arguments to forward.
pub(crate) fn composite_call(expr: &Expr) -> syn::Result<(&Ident, &Ident, Vec<&Expr>)> {
    let Expr::MethodCall(mc) = expr else {
        return Err(syn::Error::new(
            expr.span(),
            "a tuple binds a composite's outputs: `let (a, b) = stream.face(..);`",
        ));
    };
    if let Some(tf) = &mc.turbofish {
        return Err(syn::Error::new(
            tf.span(),
            "a composite call takes no type arguments",
        ));
    }
    let Expr::Path(p) = &*mc.receiver else {
        return Err(syn::Error::new(
            mc.receiver.span(),
            format!(
                "`.{}(..)` is bound to a tuple, so it is a composite and must be called directly \
                 on a bound stream (or the builder) — bind the chain before it to a `let` first",
                mc.method
            ),
        ));
    };
    let Some(receiver) = p.path.get_ident() else {
        return Err(syn::Error::new(
            p.span(),
            "a composite's receiver is a bound stream name or the builder",
        ));
    };
    Ok((receiver, &mc.method, mc.args.iter().collect()))
}

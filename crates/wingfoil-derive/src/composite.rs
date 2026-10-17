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
use quote::{ToTokens, format_ident, quote};
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

        let mut bind = Binder::new(name, builder, k);
        let (first, first_kind) = &self.params[0];
        bind.receiver(first, *first_kind, receiver, &is_stream)?;
        for ((param, kind), arg) in self.params[1..].iter().zip(args) {
            let substitute = self.inline_closures.contains(&param.to_string());
            bind.arg(param, *kind, arg, substitute, &is_stream)?;
        }

        // Every name the body binds, renamed; the returned ones to the call
        // site's pattern, a `_` keeping its fresh name.
        let returned: HashMap<String, &Option<Ident>> =
            self.tail.iter().map(Ident::to_string).zip(outs).collect();
        for b in &self.bound {
            let to = match returned.get(&b.to_string()) {
                Some(Some(out)) => out.clone(),
                _ => bind.fresh(b),
            };
            bind.rename.insert(b.to_string(), to);
        }

        let body: Vec<Stmt> = self
            .body
            .iter()
            .map(|stmt| {
                let mut stmt = stmt.clone();
                bind.renamer().visit_stmt_mut(&mut stmt);
                stmt
            })
            .collect();
        let mut stmts = bind.prelude;
        stmts.extend(body);
        Ok(stmts)
    }
}

/// One call's renaming, shared by composites and builder faces: what each
/// parameter becomes at the call site, and the locals plain arguments are
/// bound to ahead of the body.
struct Binder<'a> {
    face: &'a Ident,
    builder: &'a Ident,
    k: usize,
    rename: HashMap<String, Ident>,
    closures: HashMap<String, Expr>,
    prelude: Vec<Stmt>,
}

impl<'a> Binder<'a> {
    fn new(face: &'a Ident, builder: &'a Ident, k: usize) -> Self {
        Self {
            face,
            builder,
            k,
            rename: HashMap::new(),
            closures: HashMap::new(),
            prelude: Vec::new(),
        }
    }

    /// A name no other call, and no user binding, can have.
    fn fresh(&self, base: &Ident) -> Ident {
        Ident::new(&format!("wf_anon_c{}_{base}", self.k), Span::call_site())
    }

    fn renamer(&self) -> Renamer<'_> {
        Renamer {
            rename: &self.rename,
            closures: &self.closures,
        }
    }

    /// The first parameter: the call's receiver, a stream or the builder.
    fn receiver(
        &mut self,
        param: &Ident,
        kind: ParamKind,
        receiver: &Ident,
        is_stream: &impl Fn(&Ident) -> bool,
    ) -> syn::Result<()> {
        let (face, builder) = (self.face, self.builder);
        match kind {
            ParamKind::Builder if receiver != builder => {
                return Err(syn::Error::new(
                    receiver.span(),
                    format!("`{face}` is a face on the builder: call it as `{builder}.{face}(..)`"),
                ));
            }
            ParamKind::Stream if receiver == builder || !is_stream(receiver) => {
                return Err(syn::Error::new(
                    receiver.span(),
                    format!("`{face}` is a face on a stream: its receiver must be one"),
                ));
            }
            _ => {}
        }
        self.rename.insert(param.to_string(), receiver.clone());
        Ok(())
    }

    /// Any later parameter, bound to its call-site argument. `substitute`
    /// says a literal closure may be pasted where the parameter is used,
    /// rather than bound to a local: see `sole_config_params`.
    fn arg(
        &mut self,
        param: &Ident,
        kind: ParamKind,
        arg: &Expr,
        substitute: bool,
        is_stream: &impl Fn(&Ident) -> bool,
    ) -> syn::Result<()> {
        let (face, builder) = (self.face, self.builder);
        match kind {
            ParamKind::Stream | ParamKind::Builder => {
                let target = stream_arg(arg).filter(|id| match kind {
                    ParamKind::Builder => id == builder,
                    _ => is_stream(id),
                });
                let Some(target) = target else {
                    let want = if kind == ParamKind::Builder {
                        format!("the builder `{builder}`")
                    } else {
                        "`&name` naming a stream bound in this graph".to_string()
                    };
                    return Err(syn::Error::new(
                        arg.span(),
                        format!("`{face}`'s parameter `{param}` takes {want}"),
                    ));
                };
                self.rename.insert(param.to_string(), target);
            }
            ParamKind::Plain if substitute && matches!(arg, Expr::Closure(_)) => {
                self.closures.insert(param.to_string(), arg.clone());
            }
            ParamKind::Plain => {
                let local = self.fresh(param);
                self.prelude.push(syn::parse_quote! { let #local = #arg; });
                self.rename.insert(param.to_string(), local);
            }
        }
        Ok(())
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

/// A builder face: an opening call, option methods, and `.build()` —
/// `desired.wire_oms(config, epoch, &reports).trading_state(&t).build()`.
///
/// It wires nothing of its own. Its definition says what the chain *means*:
/// each option a parameter, an option left out wired from its default (a
/// closure on the builder, `|g| g.never()`), and the `build` arm whose
/// options without a default match the chain's naming the face the chain
/// stands for. Inlining binds the opening call's and the options' arguments
/// as a composite binds its own, wires each missing default, and leaves one
/// call of that face — which `nitro!` then inlines as a composite, or
/// dispatches as an op.
///
/// ```ignore
/// builder wire_oms<I>(desired: &Stream<..>, config: Config, epoch: Epoch, reports: &Stream<..>) {
///     trading_state(trading: &Stream<TradingState>) = |g| g.never().map(|(): &()| TradingState::Open);
///     cancel_all(cancel_all: &Stream<()>) = |g| g.never();
///     sweep(sweep: &Stream<()>) = |g| g.never();
///     reading(read: F);
///     build() => desired.oms(config, epoch, reports, trading, cancel_all, sweep);
///     build(reading) => desired.oms_reading(config, epoch, reports, trading, cancel_all, sweep, read);
/// }
/// ```
///
/// The fluent builder (the typestate struct a face's trait method returns)
/// stays hand-written, and must wire what this says: `build()` calling the
/// arm's face function, and each default the same chain. The parity tests
/// hold the two together.
pub(crate) struct BuilderDef {
    /// The definition, verbatim, to hand on to the next pass.
    tokens: TokenStream2,
    name: Ident,
    params: Vec<(Ident, ParamKind)>,
    options: Vec<BuilderOption>,
    arms: Vec<BuildArm>,
    /// The plain parameters a literal closure may pass straight through:
    /// every arm's face call takes each as its only config argument.
    inline_closures: HashSet<String>,
}

struct BuilderOption {
    method: Ident,
    param: Ident,
    kind: ParamKind,
    /// `|g| <chain>`, wired when the chain leaves the option out.
    default: Option<syn::ExprClosure>,
}

struct BuildArm {
    /// The options without a default this arm is for, sorted.
    needs: Vec<String>,
    target: Expr,
}

impl Parse for BuilderDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // `Def::parse` hands this its own tokens only.
        let tokens: TokenStream2 = input.fork().parse()?;
        let kw: Ident = input.parse()?;
        if kw != "builder" {
            return Err(syn::Error::new(
                kw.span(),
                "expected `builder <name>(..) { .. }`",
            ));
        }
        let name: Ident = input.parse()?;
        if !name.to_string().starts_with(BUILDER_PREFIX) {
            return Err(syn::Error::new(
                name.span(),
                format!(
                    "a builder face is named `{BUILDER_PREFIX}<face>` — the name is how `nitro!` \
                     tells its chain from `.build()` typed by habit"
                ),
            ));
        }
        let _generics: syn::Generics = input.parse()?;
        let content;
        syn::parenthesized!(content in input);
        let params = content
            .parse_terminated(FnArg::parse, Token![,])?
            .iter()
            .map(typed_param)
            .collect::<syn::Result<Vec<_>>>()?;
        match params.first() {
            Some((_, ParamKind::Plain)) | None => {
                return Err(syn::Error::new(
                    name.span(),
                    "a builder face's first parameter is its receiver: `&Stream<T>`, or \
                     `&GraphBuilder`",
                ));
            }
            _ => {}
        }
        let body;
        syn::braced!(body in input);
        let (mut options, mut arms) = (Vec::<BuilderOption>::new(), Vec::<BuildArm>::new());
        while !body.is_empty() {
            let method: Ident = body.parse()?;
            let inner;
            syn::parenthesized!(inner in body);
            if method == "build" {
                let mut needs: Vec<String> = inner
                    .parse_terminated(Ident::parse, Token![,])?
                    .iter()
                    .map(Ident::to_string)
                    .collect();
                needs.sort();
                body.parse::<Token![=>]>()?;
                let target: Expr = body.parse()?;
                composite_call(&target)?;
                if arms.iter().any(|a| a.needs == needs) {
                    return Err(syn::Error::new(
                        method.span(),
                        format!("two `build({})` arms", needs.join(", ")),
                    ));
                }
                arms.push(BuildArm { needs, target });
            } else {
                let arg: FnArg = inner.parse()?;
                let (param, kind) = typed_param(&arg)?;
                let default = if body.peek(Token![=]) && !body.peek(Token![=>]) {
                    body.parse::<Token![=]>()?;
                    let mut closure: syn::ExprClosure = body.parse()?;
                    // `|g| { g.never() }` is `|g| g.never()`: the default is
                    // inlined as a wiring chain, and a block is not one.
                    if let Expr::Block(block) = &*closure.body
                        && let [Stmt::Expr(chain, None)] = &block.block.stmts[..]
                    {
                        closure.body = Box::new(chain.clone());
                    }
                    if closure.inputs.len() != 1 || !matches!(closure.inputs[0], Pat::Ident(_)) {
                        return Err(syn::Error::new(
                            closure.span(),
                            "an option's default is a closure on the builder: `|g| g.never()`",
                        ));
                    }
                    Some(closure)
                } else {
                    None
                };
                if options.iter().any(|o| o.method == method) {
                    return Err(syn::Error::new(
                        method.span(),
                        format!("option `{method}` is declared twice"),
                    ));
                }
                options.push(BuilderOption {
                    method,
                    param,
                    kind,
                    default,
                });
            }
            body.parse::<Token![;]>()?;
        }
        for arm in &arms {
            for need in &arm.needs {
                if !options
                    .iter()
                    .any(|o| o.method == need.as_str() && o.default.is_none())
                {
                    return Err(syn::Error::new(
                        name.span(),
                        format!(
                            "`build({need})` names no option without a default — an arm is \
                             chosen by which of those the chain names"
                        ),
                    ));
                }
            }
        }
        // An arm's call may read the opening parameters, every option with a
        // default, and the options it is chosen by — nothing else: an option
        // without a default that the arm does not list is unbound there.
        for arm in &arms {
            struct Names(Vec<Ident>);
            impl<'ast> syn::visit::Visit<'ast> for Names {
                fn visit_expr_path(&mut self, p: &'ast syn::ExprPath) {
                    self.0.extend(p.path.get_ident().cloned());
                }
            }
            let mut names = Names(Vec::new());
            syn::visit::Visit::visit_expr(&mut names, &arm.target);
            for used in names.0 {
                if let Some(option) = options
                    .iter()
                    .find(|o| o.param == used && o.default.is_none())
                    && !arm.needs.contains(&option.method.to_string())
                {
                    return Err(syn::Error::new(
                        used.span(),
                        format!(
                            "this arm reads `{used}`, but it is the option `.{}(..)` without a \
                             default and the arm is not `build({})` — list it, or give it a \
                             default",
                            option.method, option.method
                        ),
                    ));
                }
            }
        }
        if arms.is_empty() {
            return Err(syn::Error::new(
                name.span(),
                "a builder face needs a `build() => ..` arm",
            ));
        }
        let mut edges: HashSet<String> = HashSet::new();
        let mut plain: Vec<&Ident> = Vec::new();
        let all = params
            .iter()
            .map(|(p, k)| (p, *k))
            .chain(options.iter().map(|o| (&o.param, o.kind)));
        for (p, k) in all {
            if k == ParamKind::Plain {
                plain.push(p);
            } else {
                edges.insert(p.to_string());
            }
        }
        let targets: Vec<Stmt> = arms
            .iter()
            .map(|a| Stmt::Expr(a.target.clone(), Some(Token![;](a.target.span()))))
            .collect();
        let inline_closures = sole_config_params(&targets, &plain, &edges);
        Ok(Self {
            tokens,
            name,
            params,
            options,
            arms,
            inline_closures,
        })
    }
}

impl BuilderDef {
    /// The arm a chain naming `named` builds: the one whose options without
    /// a default are exactly the ones named.
    fn arm(&self, named: &[&Ident]) -> syn::Result<&BuildArm> {
        let mut needs: Vec<String> = named
            .iter()
            .filter(|m| {
                self.options
                    .iter()
                    .any(|o| o.method == ***m && o.default.is_none())
            })
            .map(|m| m.to_string())
            .collect();
        needs.sort();
        self.arms.iter().find(|a| a.needs == needs).ok_or_else(|| {
            syn::Error::new(
                self.name.span(),
                format!("`{}` has no `build({})` arm", self.name, needs.join(", ")),
            )
        })
    }

    /// The face the chain stands for: its method, for the prepass.
    pub(crate) fn target(&self, named: &[&Ident]) -> syn::Result<&Ident> {
        Ok(composite_call(&self.arm(named)?.target)?.1)
    }

    /// Inline one chain: the default wiring for every option it leaves out,
    /// and `let <pat> = <the arm's face call>;`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn inline(
        &self,
        receiver: &Ident,
        builder: &Ident,
        open: &[&Expr],
        named: &[(&Ident, Vec<&Expr>)],
        pat: &Pat,
        k: usize,
        is_stream: impl Fn(&Ident) -> bool,
    ) -> syn::Result<Vec<Stmt>> {
        let name = &self.name;
        if open.len() + 1 != self.params.len() {
            return Err(syn::Error::new(
                receiver.span(),
                format!(
                    "`{name}` takes {} argument(s) after its receiver; this call passes {}",
                    self.params.len() - 1,
                    open.len()
                ),
            ));
        }
        let mut bind = Binder::new(name, builder, k);
        let (first, first_kind) = &self.params[0];
        bind.receiver(first, *first_kind, receiver, &is_stream)?;
        for ((param, kind), arg) in self.params[1..].iter().zip(open) {
            let substitute = self.inline_closures.contains(&param.to_string());
            bind.arg(param, *kind, arg, substitute, &is_stream)?;
        }

        let mut given: Vec<&Ident> = Vec::new();
        for (method, args) in named {
            let Some(option) = self.options.iter().find(|o| o.method == **method) else {
                let known: Vec<String> = self
                    .options
                    .iter()
                    .map(|o| format!("`.{}(..)`", o.method))
                    .collect();
                return Err(syn::Error::new(
                    method.span(),
                    format!(
                        "`{name}` has no option `.{method}(..)`; it takes {} and then `.build()`",
                        known.join(", ")
                    ),
                ));
            };
            if given.contains(method) {
                return Err(syn::Error::new(
                    method.span(),
                    format!("`.{method}(..)` is named twice"),
                ));
            }
            let [arg] = args[..] else {
                return Err(syn::Error::new(
                    method.span(),
                    format!("`.{method}(..)` takes one argument"),
                ));
            };
            let substitute = self.inline_closures.contains(&option.param.to_string());
            bind.arg(&option.param, option.kind, arg, substitute, &is_stream)?;
            given.push(method);
        }
        let arm = self.arm(&given)?;

        // Each option left out with a default: its chain, rooted at the graph's
        // builder, bound to a fresh name the arm's call then reads.
        let mut defaults: Vec<Stmt> = Vec::new();
        for option in &self.options {
            if given.iter().any(|m| **m == option.method) {
                continue;
            }
            let Some(default) = &option.default else {
                continue;
            };
            let Pat::Ident(g) = &default.inputs[0] else {
                unreachable!("checked at parse");
            };
            let local = bind.fresh(&option.param);
            let mut chain = (*default.body).clone();
            let mut rename = bind.rename.clone();
            rename.insert(g.ident.to_string(), builder.clone());
            Renamer {
                rename: &rename,
                closures: &bind.closures,
            }
            .visit_expr_mut(&mut chain);
            defaults.push(syn::parse_quote! { let #local = #chain; });
            bind.rename.insert(option.param.to_string(), local);
        }
        let mut target = arm.target.clone();
        bind.renamer().visit_expr_mut(&mut target);

        let mut stmts = bind.prelude;
        stmts.extend(defaults);
        stmts.push(syn::parse_quote! { let #pat = #target; });
        Ok(stmts)
    }
}

/// A definition `nitro!` can be handed: a composite face, or a builder face.
pub(crate) enum Def {
    Face(Box<CompositeDef>),
    Builder(BuilderDef),
}

impl Def {
    pub(crate) fn name(&self) -> &Ident {
        match self {
            Def::Face(f) => f.name(),
            Def::Builder(b) => &b.name,
        }
    }
}

impl Parse for Def {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.peek(Ident) && input.fork().parse::<Ident>().is_ok_and(|i| i == "builder") {
            // A builder face's own tokens — through its `{ .. }` body, which
            // is the only brace group in it — and nothing after: the
            // definition is handed on verbatim, and `@composites [..]` lists
            // more definitions behind it.
            let mut tokens = TokenStream2::new();
            loop {
                let tt: proc_macro2::TokenTree = input.parse()?;
                let body = matches!(&tt, proc_macro2::TokenTree::Group(g)
                    if g.delimiter() == proc_macro2::Delimiter::Brace);
                tokens.extend([tt]);
                if body {
                    break;
                }
            }
            return Ok(Def::Builder(syn::parse2(tokens)?));
        }
        Ok(Def::Face(Box::new(CompositeDef::new(input.parse()?)?)))
    }
}

impl quote::ToTokens for Def {
    fn to_tokens(&self, out: &mut TokenStream2) {
        match self {
            Def::Face(f) => f.item.to_tokens(out),
            Def::Builder(b) => b.tokens.to_tokens(out),
        }
    }
}

/// The prefix every builder face's name carries — the convention that tells
/// a builder chain from `.build()` typed by habit.
pub(crate) const BUILDER_PREFIX: &str = "wire_";

/// `recv.wire_face(open..).option(arg)...build()`, taken apart: the receiver,
/// the face, its opening arguments, and each option call in order.
///
/// The tell is the shape *and* the name: an argument-less `.build()` ending a
/// chain whose first call is `wire_<face>`. `build` alone cannot be it —
/// `.print().build()`, carried in from fluent code by habit, ends a chain
/// too, and keeps the advice `Stream::build` gets (`non_op_method_advice`)
/// rather than becoming a call to a shell nobody wrote. `None` for anything
/// else.
#[allow(clippy::type_complexity)]
pub(crate) fn builder_chain(
    expr: &Expr,
) -> Option<(&Ident, &Ident, Vec<&Expr>, Vec<(&Ident, Vec<&Expr>)>)> {
    let Expr::MethodCall(build) = expr else {
        return None;
    };
    if build.method != "build" || !build.args.is_empty() {
        return None;
    }
    let mut calls = Vec::new();
    let mut cur = &*build.receiver;
    let root = loop {
        match cur {
            Expr::MethodCall(mc) if mc.turbofish.is_none() => {
                calls.push((&mc.method, mc.args.iter().collect::<Vec<_>>()));
                cur = &mc.receiver;
            }
            Expr::Path(p) if p.qself.is_none() => break p.path.get_ident()?,
            _ => return None,
        }
    };
    let (face, open) = calls.pop()?;
    if !face.to_string().starts_with(BUILDER_PREFIX) {
        return None;
    }
    calls.reverse();
    Some((root, face, open, calls))
}

/// `composite!`'s input: `[<callback>] <definition>`.
pub(crate) struct CompositeInput {
    callback: TokenStream2,
    def: Def,
}

impl Parse for CompositeInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let content;
        syn::bracketed!(content in input);
        let callback: TokenStream2 = content.parse()?;
        let def: Def = input.parse()?;
        if !input.is_empty() {
            return Err(input.error("composite! takes exactly one definition"));
        }
        Ok(Self { callback, def })
    }
}

/// What `composite!` expands to: the definition itself under `define` (a
/// builder face's is nothing — its fluent builder is hand-written), or the
/// next `nitro!` pass with the definition added under `@nitro`.
pub(crate) fn expand(input: CompositeInput) -> syn::Result<TokenStream2> {
    let item = &input.def;
    let mut cb = input.callback.clone().into_iter();
    match (cb.next(), cb.next(), cb.next()) {
        (Some(proc_macro2::TokenTree::Ident(id)), None, None) if id == "define" => {
            Ok(match &input.def {
                Def::Face(f) => f.item.to_token_stream(),
                Def::Builder(_) => TokenStream2::new(),
            })
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

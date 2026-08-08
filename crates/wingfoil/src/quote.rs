//! Closure **quotation**: keeping a closure's source text alongside the
//! closure, so a wired graph can be asked what its nodes actually compute.
//!
//! The interpreted engine erases closures — a node's cycle is a
//! `Box<dyn FnMut(&mut Kernel)>` over `Rc<dyn Any>` value slots — so traversing
//! a wired graph recovers its *topology* but never its closure *bodies*. That
//! erasure is the wall the deleted legacy `codegen` retrofit hit, and the one
//! [`func!`] gets over: a macro wrapped around the closure **at its call site**
//! sees the tokens before type-checking, and keeps them.
//!
//! Two consequences, the second load-bearing:
//!
//! 1. **Source recovery** — the graph can report what each node computes.
//! 2. **No drift is possible.** The string and the closure come from the *same
//!    tokens*, so what a reader (or a generator) sees is what ran. The legacy
//!    retrofit's failure mode — a human re-stating each closure by hand, and
//!    getting one wrong — cannot occur here by construction.
//!
//! Design: `docs/wired-graph-codegen-decision.md` §2–§4; sequencing: #726.
//! This is step 2, worth having on its own merits (graph introspection and
//! debugging) whether or not the generator in step 3 is ever built.
//!
//! # How it attaches: one method, not a parallel op surface
//!
//! §4 of the decision doc proposes binding every closure-config op by an
//! `OpFn` trait with two implementors, so `map` and friends accept a plain or a
//! quoted closure through one signature. **That does not work, and the reason
//! is worth recording**: rustc propagates closure *signature* inference only
//! from `Fn`/`FnMut`/`FnOnce` bounds. Behind any other trait a closure literal
//! loses both parameter-type inference and higher-ranked lifetime inference, so
//! every `.map(|c| *c + 100)` in the codebase needs an explicit annotation.
//! Measured on the catalog: ~370 errors across 41 targets, and — decisively —
//! the residue after fixing the fluent layer is entirely *inside* `nitro!`
//! blocks, because `compiled()` emits closure literals into forwarders whose
//! bounds come from the op. The macro's whole inference-rooting mechanism
//! depends on that bound being an `Fn` bound.
//!
//! So the ops keep their `Fn` bounds and never see a [`QuotedFn`]. Quotation is
//! unwrapped one level earlier, at the fluent layer, and the source is recorded
//! against the **node**:
//!
//! ```
//! use wingfoil::prelude::*;
//! use wingfoil::func;
//!
//! let g = GraphBuilder::new();
//! let ticks = g.ticker(std::time::Duration::from_millis(1)).count();
//!
//! let double = func!(|i: &u64| i * 2);
//! let doubled = ticks.map(double.f).with_src(&double);
//!
//! assert_eq!(Some("|i: &u64| i * 2"), doubled.src().as_deref());
//! ```
//!
//! That is one new method ([`Stream::with_src`](crate::fluent::Stream::with_src))
//! covering *every* op — built-in and user-defined alike — instead of a quoted
//! twin per op. It also puts the metadata where a traversal actually looks:
//! on the node, not on the config value. `nitro!` needs none of this (§4: "the
//! macro has the tokens already"), which is why nothing on the compiled path
//! changes.

use std::fmt;

/// One captured binding, recorded by name and by **value**.
///
/// The value is rendered through [`EmitLiteral`](crate::emit::EmitLiteral) at
/// quotation time, so a
/// generator can re-materialise the binding wherever it splices the body:
/// `{ let fee = 2.5f64; move |p| p * fee }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capture {
    /// The binding's name, exactly as the closure body refers to it.
    pub name: &'static str,
    /// Its value at quotation time, as Rust source.
    pub value: String,
}

/// A closure paired with the source text it was written as.
///
/// Constructed only by [`func!`], which is what makes the two agree. Pass the
/// closure on as `q.f` and record the quotation with
/// [`Stream::with_src`](crate::fluent::Stream::with_src).
#[derive(Clone)]
pub struct QuotedFn<F> {
    /// The real closure — handed to the op exactly as an unquoted one would
    /// be, so nothing about execution changes.
    pub f: F,
    /// `stringify!` of the same tokens: verbatim source, spacing intact.
    pub src: &'static str,
    /// `(file, line)` where it was written, for mapping a node back to the
    /// wiring that produced it.
    pub loc: (&'static str, u32),
    /// The bindings the closure captures, by name and rendered value. Empty
    /// for a closed closure — the common case.
    pub captures: Vec<Capture>,
}

impl<F> QuotedFn<F> {
    /// The closure as a self-contained expression: the body if it is closed,
    /// or a block that re-materialises each capture first.
    ///
    /// ```
    /// use wingfoil::func;
    ///
    /// let fee = 2.5f64;
    /// let q = func!([fee] move |p: &f64| p * fee);
    /// assert_eq!(
    ///     "{ let fee = 2.5f64; move |p: &f64| p * fee }",
    ///     q.emittable_src(),
    /// );
    /// ```
    ///
    /// This is what makes a capturing closure splice-able at all. The body
    /// alone refers to `fee`, which resolves only where it was written; wrapped
    /// in its own bindings it resolves anywhere — at the cost §3 names, that
    /// the values are **frozen** at quotation time.
    pub fn emittable_src(&self) -> String {
        if self.captures.is_empty() {
            return self.src.to_string();
        }
        let lets: Vec<String> = self
            .captures
            .iter()
            .map(|c| format!("let {} = {};", c.name, c.value))
            .collect();
        format!("{{ {} {} }}", lets.join(" "), self.src)
    }
}

impl<F> fmt::Debug for QuotedFn<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "QuotedFn({} @ {}:{})", self.src, self.loc.0, self.loc.1)
    }
}

/// Quote a closure: keep its tokens alongside it.
///
/// ```
/// use wingfoil::func;
///
/// let q = func!(|x: &f64| x * 2.0);
/// assert_eq!("|x: &f64| x * 2.0", q.src);
/// assert_eq!(8.0, (q.f)(&4.0));
/// ```
///
/// Works at every arity the catalog uses (`map`'s `Fn(&A) -> B`, `join`'s
/// `Fn(&A, &B) -> C`, `fold`'s `Fn(&mut S, &A)`, …) because it captures the
/// closure as a single expression rather than destructuring its parameter list.
///
/// That also preserves the **verbatim** source: `stringify!` on an `$f:expr`
/// metavariable returns the original snippet with its spacing, not the token
/// stream re-printed as `| x : & f64 | x * 2.0`. Which is the point — §5 wants
/// a generated artifact to be reviewable plain Rust.
///
/// # Captures — tier 1 and tier 2
///
/// A **closed** closure needs nothing extra: its body resolves anywhere.
///
/// A **capturing** closure does not. `move |p| p * fee` refers to `fee`, which
/// exists only where it was written; splice that body elsewhere and it fails to
/// resolve, or worse, resolves to a different binding. So a capture must be
/// declared, and its *value* recorded:
///
/// ```
/// use wingfoil::func;
///
/// let fee = 2.5f64;
/// let q = func!([fee] move |p: &f64| p * fee);
///
/// assert_eq!(7.5, (q.f)(&3.0));
/// assert_eq!(
///     "{ let fee = 2.5f64; move |p: &f64| p * fee }",
///     q.emittable_src(),
/// );
/// ```
///
/// Each name in the list is rendered through
/// [`EmitLiteral`](crate::emit::EmitLiteral), so captures are bounded by what
/// that trait covers — primitives, `String`, `Duration`, `NanoTime`, and their
/// containers. A capture of some arbitrary struct is a compile error at the
/// `func!` call site, which is where it can be understood.
///
/// **This freezes the capture** (§3). The emitted block carries the value the
/// closure was quoted with, so changing it means regenerating. That is partial
/// evaluation and usually the point — a per-instrument fee baked into a
/// per-instrument pipeline — but it is also how a stale threshold ships.
///
/// An **undeclared** capture is not detected here: `func!(move |p| p * fee)`
/// records a body that will not resolve elsewhere. §3 catches this by coercing
/// the expansion through a fn pointer, which this macro cannot do — the
/// coercion has to name an arity (`fn(&_) -> _`), which would make `func!`
/// unusable for `join` and `fold`. The check therefore lives where it bites:
/// pass 2 fails to compile the artifact, with a breadcrumb pointing at the
/// wiring. Recorded in `docs/deviation-register.md`.
#[macro_export]
macro_rules! func {
    // Tier 2: an explicit capture list. Each name is recorded by *value*,
    // rendered through `EmitLiteral`, so the generator can re-materialise the
    // binding where it splices the body.
    ([$($cap:ident),+ $(,)?] $f:expr) => {
        $crate::quote::QuotedFn {
            f: $f,
            src: stringify!($f),
            loc: (file!(), line!()),
            captures: ::std::vec![$(
                $crate::quote::Capture {
                    name: stringify!($cap),
                    value: $crate::emit::EmitLiteral::emit_literal(&$cap),
                }
            ),+],
        }
    };
    // Tier 1: a closed closure.
    ($f:expr) => {
        $crate::quote::QuotedFn {
            f: $f,
            src: stringify!($f),
            loc: (file!(), line!()),
            captures: ::std::vec::Vec::new(),
        }
    };
}

/// Wire one node and record its closure in a single step.
///
/// The two-step form is correct but forgettable, and forgetting is **silent**:
/// the graph runs fine interpreted, and the omission only surfaces later as a
/// generator refusal.
///
/// ```
/// use wingfoil::prelude::*;
/// use wingfoil::{func, quoted};
/// # let g = GraphBuilder::new();
/// # let ticks = g.ticker(std::time::Duration::from_millis(1)).count();
/// // Two steps, easy to half-do:
/// let double = func!(|i: &u64| i * 2);
/// let a = ticks.map(double.f).with_src(&double);
///
/// // One step, no opportunity to forget:
/// let b = quoted!(ticks => map(|i: &u64| i * 2));
///
/// assert_eq!(a.src(), b.src());
/// ```
///
/// Also takes a leading argument, for the ops that have one — a stream edge
/// (`join`) or a seed (`fold`):
///
/// ```
/// use wingfoil::prelude::*;
/// use wingfoil::quoted;
/// # let g = GraphBuilder::new();
/// # let a = g.ticker(std::time::Duration::from_millis(1)).count();
/// let b = quoted!(a => map(|i: &u64| i * 10));
/// let joined = quoted!(a => join(&b, |x: &u64, y: &u64| x + y));
/// let total = quoted!(joined => fold(0u64, |acc: &mut u64, v: &u64| *acc += v));
/// ```
///
/// # Why `=>` rather than a plain method chain
///
/// `quoted!(ticks.map(..))` would read better, and it cannot be built. A
/// `macro_rules!` pattern cannot destructure `receiver.method(args)` — an
/// `expr` fragment may only be followed by `=>`, `,` or `;`, never `.`.
///
/// A **proc** macro can parse the chain, and that was tried first. It loses the
/// one property that makes quotation worth having: `stringify!` recovers
/// verbatim source only for a fragment that came straight from a user's file,
/// and tokens re-emitted by a proc macro fall back to pretty-printing. The
/// artifact then carries `| i : & u64 | i * 2` instead of `|i: &u64| i * 2`,
/// and `rustfmt` will not fix it — it does not format inside a macro body.
/// Since a generated artifact exists to be read, the syntax cost is the better
/// trade. `Span::source_text` would settle it, but joining spans across an
/// expression is nightly-only.
///
/// # One rule for all three vocabularies
///
/// Everything a generator needs recorded is marked in the argument list:
///
/// | Write | Records |
/// |---|---|
/// | `map(\|x\| ..)` | the closure |
/// | `map([fee] move \|x\| ..)` | the closure **and** the captured value |
/// | `ticker(cfg period)` | the data config |
/// | `fold(cfg 0u64, \|a, v\| ..)` | both |
/// | `join(&other, \|x, y\| ..)` | the closure; `&other` is an edge, left alone |
///
/// So the whole per-instrument shape the generator exists for reads without
/// leaving the macro:
///
/// ```
/// use wingfoil::prelude::*;
/// use wingfoil::quoted;
/// # use std::time::Duration;
/// # struct Instrument { tick: Duration, fee: f64 }
/// # let cfg = [Instrument { tick: Duration::from_millis(1), fee: 2.5 }];
/// # let g = GraphBuilder::new();
/// for inst in &cfg {
///     let fee = inst.fee;
///     let ticks = quoted!(g => ticker(cfg inst.tick)).count();
///     let px = quoted!(ticks => map(|n: &u64| *n as f64));
///     let net = quoted!(px => map([fee] move |p: &f64| p - fee));
///     assert!(net.src().is_some());
/// }
/// ```
///
/// A `cfg`-marked argument is **cloned** into the call so the original can be
/// rendered — every [`EmitLiteral`](crate::emit::EmitLiteral) type is `Clone`,
/// and for the common `Copy` ones (`Duration`, numbers) the clone is free.
///
/// # What it does not do
///
/// A `&stream` edge is passed through untouched — there is nothing to record
/// about an edge, the graph already knows it.
///
/// Nothing here detects an *undeclared* capture. `map(move |p| p - fee)` with
/// no `[fee]` records a body that will not resolve in an artifact; it surfaces
/// as a generator refusal, or a pass-2 compile error if the name happens to
/// resolve to something else.
#[macro_export]
macro_rules! quoted {
    // Arms are ordered most-specific first, and each is discriminated by a
    // *literal* token (`cfg`, `[`) before any fragment is parsed. That matters:
    // `macro_rules!` falls through cleanly on a literal mismatch, but a failed
    // `$x:expr` parse is a hard error, not a fallthrough — so an arm that could
    // swallow another's input must come second.

    // --- data config + captured closure -------------------------------------
    ($recv:expr => $m:ident(cfg $a:expr, [$($cap:ident),+ $(,)?] $f:expr)) => {{
        let __wf_c = $a;
        let __wf_q = $crate::func!([$($cap),+] $f);
        $recv.$m(__wf_c.clone(), __wf_q.f)
            .with_cfg(&__wf_c)
            .with_src(&__wf_q)
    }};
    // --- data config + closure ----------------------------------------------
    ($recv:expr => $m:ident(cfg $a:expr, $f:expr)) => {{
        let __wf_c = $a;
        let __wf_q = $crate::func!($f);
        $recv.$m(__wf_c.clone(), __wf_q.f)
            .with_cfg(&__wf_c)
            .with_src(&__wf_q)
    }};
    // --- data config only (a source: `ticker`, `limit`) ---------------------
    ($recv:expr => $m:ident(cfg $a:expr)) => {{
        let __wf_c = $a;
        $recv.$m(__wf_c.clone()).with_cfg(&__wf_c)
    }};
    // --- leading argument + captured closure --------------------------------
    ($recv:expr => $m:ident($a:expr, [$($cap:ident),+ $(,)?] $f:expr)) => {{
        let __wf_q = $crate::func!([$($cap),+] $f);
        $recv.$m($a, __wf_q.f).with_src(&__wf_q)
    }};
    // --- leading argument + closure (a stream edge for `join`, a seed) ------
    ($recv:expr => $m:ident($a:expr, $f:expr)) => {{
        let __wf_q = $crate::func!($f);
        $recv.$m($a, __wf_q.f).with_src(&__wf_q)
    }};
    // --- captured closure ---------------------------------------------------
    ($recv:expr => $m:ident([$($cap:ident),+ $(,)?] $f:expr)) => {{
        let __wf_q = $crate::func!([$($cap),+] $f);
        $recv.$m(__wf_q.f).with_src(&__wf_q)
    }};
    // --- closure ------------------------------------------------------------
    ($recv:expr => $m:ident($f:expr)) => {{
        let __wf_q = $crate::func!($f);
        $recv.$m(__wf_q.f).with_src(&__wf_q)
    }};
}

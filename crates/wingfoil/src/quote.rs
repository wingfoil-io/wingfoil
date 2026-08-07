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
//! assert_eq!(Some("|i: &u64| i * 2"), doubled.src());
//! ```
//!
//! That is one new method ([`Stream::with_src`](crate::fluent::Stream::with_src))
//! covering *every* op — built-in and user-defined alike — instead of a quoted
//! twin per op. It also puts the metadata where a traversal actually looks:
//! on the node, not on the config value. `nitro!` needs none of this (§4: "the
//! macro has the tokens already"), which is why nothing on the compiled path
//! changes.

use std::fmt;

/// A closure paired with the source text it was written as.
///
/// Constructed only by [`func!`], which is what makes the two agree. Pass the
/// closure on as `q.f` and record the quotation with
/// [`Stream::with_src`](crate::fluent::Stream::with_src).
#[derive(Clone, Copy)]
pub struct QuotedFn<F> {
    /// The real closure — handed to the op exactly as an unquoted one would
    /// be, so nothing about execution changes.
    pub f: F,
    /// `stringify!` of the same tokens: verbatim source, spacing intact.
    pub src: &'static str,
    /// `(file, line)` where it was written, for mapping a node back to the
    /// wiring that produced it.
    pub loc: (&'static str, u32),
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
/// # Captures
///
/// A quoted closure may capture, and the capture is **not** recorded, so `src`
/// is then a body referencing names that exist only at the original site. Fine
/// for introspection — the text is still what ran — but such a node is not
/// *emittable*: splicing the body elsewhere would fail to resolve, or worse,
/// resolve to a different binding.
///
/// §3 enforces closedness by coercing the expansion through a fn pointer, which
/// rejects capturing closures at the call site. That is **not done here**,
/// deliberately: the coercion has to name an arity (`fn(&_) -> _`), so it would
/// have made `func!` unusable for `join` and `fold`. Closedness is therefore
/// checked where the requirement actually bites — in the generator, which knows
/// it is about to splice — and §3's tier-2 explicit capture lists remain the
/// route to emittable captures. See `docs/deviation-register.md`.
#[macro_export]
macro_rules! func {
    ($f:expr) => {
        $crate::quote::QuotedFn {
            f: $f,
            src: stringify!($f),
            loc: (file!(), line!()),
        }
    };
}

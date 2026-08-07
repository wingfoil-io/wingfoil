//! **SPIKE — not production surface.** User-level quotation of closures, the
//! one new primitive the two-pass codegen design turns on
//! (`docs/wired-graph-codegen-decision.md` §2, tracked as #726).
//!
//! The problem it exists to solve: a wired graph gives back its *topology* by
//! traversal, but not its closure *bodies* — the interpreted engine erases them
//! into `Box<dyn FnMut>` cycles over `Rc<dyn Any>` slots. A generator meeting a
//! closure after erasure cannot recover its source, and cannot even name its
//! type to hand it back to a forwarder. [`func!`] keeps the closure's tokens
//! alongside the closure itself, at the call site, before either erasure
//! happens.
//!
//! This module exists to answer **one question**: does an op whose `Cfg` is
//! bound by [`OpFn`] rather than `Fn` still monomorphize through `nitro!`'s
//! compiled emission? Two things could break:
//!
//! 1. **Coherence** — a blanket impl over `Fn` plus a specific impl for
//!    [`QuotedFn`] must not overlap.
//! 2. **Inference** — `nitro!`'s compiled path passes a literal-closure config
//!    *by value* as a direct argument (`op::cycle_owned_cfg`), so that rustc
//!    defers the closure's signature inference until the sibling `input`
//!    argument has resolved the op's value types. Behind `&mut` that deferral
//!    does not apply. Wrapping the closure adds a layer between the literal and
//!    that argument position.

use std::fmt;

/// A closure paired with the source text it was written as.
///
/// Constructed only by [`func!`], which is what makes the two agree: the
/// closure and the string come from the *same tokens*, so drift between the
/// value that ran and the text that would be emitted is structurally
/// impossible. That is the property the deleted legacy `codegen` retrofit
/// never had — it re-stated each closure by hand.
#[derive(Clone, Copy)]
pub struct QuotedFn<F> {
    /// The real closure. The interpreted and compiled paths call this; the
    /// quotation is dead weight after monomorphization.
    pub f: F,
    /// `stringify!` of the same tokens, for a generator to splice back out as
    /// a closure *literal* at the emitted call site.
    pub src: &'static str,
    /// Where it was written, for mapping a pass-2 compile error in generated
    /// code back to the wiring that produced it.
    pub loc: (&'static str, u32),
}

impl<F> fmt::Debug for QuotedFn<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "QuotedFn({} @ {}:{})", self.src, self.loc.0, self.loc.1)
    }
}

/// The callable an op takes as its config, abstracting over "a plain closure"
/// and "a quoted closure".
///
/// Ops bound by this instead of `Fn` accept both forms. The blanket impl
/// reports no source; [`QuotedFn`] reports its own. An op invokes the config
/// through [`call`](OpFn::call) rather than `()` syntax, because `QuotedFn`
/// deliberately does **not** implement `Fn` — that is exactly what keeps the
/// two impls from overlapping.
pub trait OpFn<A: ?Sized, B> {
    fn call(&self, arg: &A) -> B;

    /// The closure's source text, when it was quoted. `None` for a plain
    /// closure — which is the signal a generator uses to report a
    /// non-emittable node and the call site that wired it.
    fn src(&self) -> Option<&'static str> {
        None
    }

    /// Where the closure was written, when it was quoted.
    fn loc(&self) -> Option<(&'static str, u32)> {
        None
    }
}

impl<A: ?Sized, B, F> OpFn<A, B> for F
where
    F: Fn(&A) -> B,
{
    #[inline(always)]
    fn call(&self, arg: &A) -> B {
        self(arg)
    }
}

impl<A: ?Sized, B, F> OpFn<A, B> for QuotedFn<F>
where
    F: Fn(&A) -> B,
{
    #[inline(always)]
    fn call(&self, arg: &A) -> B {
        (self.f)(arg)
    }

    fn src(&self) -> Option<&'static str> {
        Some(self.src)
    }

    fn loc(&self) -> Option<(&'static str, u32)> {
        Some(self.loc)
    }
}

/// Quote a closure: keep its tokens alongside it.
///
/// ```ignore
/// let doubled = prices.quoted_map(func!(|x: &f64| x * 2.0));
/// ```
///
/// **Tier 1 only in this spike — no captures.** The expansion coerces through
/// a fn pointer, so a capturing closure fails to compile *at the call site*
/// rather than producing a body that cannot be spliced elsewhere. The design's
/// tier 2 (`func!([thresh] |x| x > thresh)`, capture values recorded through an
/// `EmitLiteral` bound) is not implemented here — it is orthogonal to the
/// question this spike asks.
#[macro_export]
macro_rules! func {
    ($f:expr) => {{
        // Coerce through a fn pointer: a non-capturing closure coerces, a
        // capturing one is a compile error *here*, where the user can see it.
        let __f: fn(&_) -> _ = $f;
        $crate::quote::QuotedFn {
            f: __f,
            src: stringify!($f),
            loc: (file!(), line!()),
        }
    }};
}

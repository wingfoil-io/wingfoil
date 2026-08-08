//! Rendering a **value** back into Rust source that reconstructs it.
//!
//! The other half of what a two-pass generator needs from a wired graph.
//! [`quote`](crate::quote) recovers *closure* configs, because closures are
//! erased and only their tokens survive. This module covers the *data* configs
//! — a `ticker`'s `Duration`, a `fold`'s seed, a `limit`'s bound — which are
//! not erased at all: the value is right there, it just has no way to say what
//! it would look like written down.
//!
//! `docs/wired-graph-codegen-decision.md` §3 names this as `EmitLiteral` and
//! bounds the emittable set to "primitives, `String`, `Duration`, `NanoTime`,
//! `Vec<primitive>`, …". Tracked as #726.
//!
//! # Why render the value rather than capture its name
//!
//! `func!` keeps *source text* (`|x| x * 2`), which is the only option for a
//! closure. The tempting symmetry would be a `lit!(PERIOD)` capturing the token
//! `PERIOD` — and it reads beautifully in a generated artifact.
//!
//! It is also wrong for the case the generator exists to serve. A name only
//! resolves if the artifact is compiled in a scope where that name is bound,
//! and the whole point of pass 1 is topology decided by **running code** —
//! periods read from a config file, thresholds from a database. Those values
//! have no name to capture. Rendering the value works for both, which is why
//! §3 specifies it, and why the same mechanism serves §3's frozen captures
//! (`{ let thresh = 0.25f64; move |x| x.abs() > thresh }`).
//!
//! The cost is the one §3 names loudly: emitting a value **freezes** it. The
//! artifact carries pass 1's configuration, so changing it means regenerate and
//! recompile. That is partial evaluation, often the point — but ship a stale
//! threshold once and you will want this sentence to have been louder.
//!
//! # Round-tripping is the contract
//!
//! Every impl must produce source that evaluates back to an equal value, and
//! each is pinned by a test that says so. The float impls are where this bites:
//! `{}` formatting loses precision, so they use `{:?}` (which is
//! round-trippable) and special-case the non-finite values, which have no
//! literal form at all.

use std::time::Duration;

use crate::runtime::time::NanoTime;

/// A value that can render itself as Rust source reconstructing it.
///
/// The output must be a self-contained *expression*, valid wherever the
/// artifact is compiled — so paths are absolute (`::core::time::Duration`),
/// never relying on the generator's imports being present at the far end.
pub trait EmitLiteral {
    /// Rust source that evaluates to a value equal to `self`.
    fn emit_literal(&self) -> String;
}

/// Integers and other types whose `Display` is already a valid literal, given a
/// type suffix. The suffix is not decoration: an unsuffixed `1` in a generated
/// artifact infers to `i32` and mismatches a `u64` config.
macro_rules! emit_suffixed {
    ($($t:ty => $suffix:literal),* $(,)?) => {$(
        impl EmitLiteral for $t {
            fn emit_literal(&self) -> String {
                format!(concat!("{}", $suffix), self)
            }
        }
    )*};
}

emit_suffixed! {
    u8 => "u8", u16 => "u16", u32 => "u32", u64 => "u64", u128 => "u128", usize => "usize",
    i8 => "i8", i16 => "i16", i32 => "i32", i64 => "i64", i128 => "i128", isize => "isize",
}

/// Floats need `{:?}`, not `{}`: `Display` truncates (`0.1 + 0.2` prints as
/// `0.30000000000000004` either way, but plenty of values do not survive
/// `Display`), while `Debug` is specified to round-trip. The non-finite values
/// have no literal form and must be named.
macro_rules! emit_float {
    ($($t:ty => $suffix:literal, $path:literal),* $(,)?) => {$(
        impl EmitLiteral for $t {
            fn emit_literal(&self) -> String {
                if self.is_nan() {
                    format!("{}::NAN", $path)
                } else if *self == <$t>::INFINITY {
                    format!("{}::INFINITY", $path)
                } else if *self == <$t>::NEG_INFINITY {
                    format!("{}::NEG_INFINITY", $path)
                } else {
                    format!(concat!("{:?}", $suffix), self)
                }
            }
        }
    )*};
}

emit_float! {
    f32 => "f32", "::core::primitive::f32",
    f64 => "f64", "::core::primitive::f64",
}

impl EmitLiteral for bool {
    fn emit_literal(&self) -> String {
        if *self { "true" } else { "false" }.to_string()
    }
}

impl EmitLiteral for char {
    fn emit_literal(&self) -> String {
        format!("{self:?}")
    }
}

impl EmitLiteral for str {
    /// `{:?}` on a `str` is Rust's own escaping, so quotes, backslashes and
    /// control characters come back out as a valid literal.
    fn emit_literal(&self) -> String {
        format!("{self:?}")
    }
}

impl EmitLiteral for String {
    fn emit_literal(&self) -> String {
        format!("{:?}.to_string()", self.as_str())
    }
}

impl EmitLiteral for Duration {
    /// `Duration::new(secs, nanos)` rather than `from_millis` and friends: it is
    /// exact for every representable value, where the convenience constructors
    /// only round-trip durations that happen to land on their unit.
    fn emit_literal(&self) -> String {
        format!(
            "::core::time::Duration::new({}u64, {}u32)",
            self.as_secs(),
            self.subsec_nanos()
        )
    }
}

impl EmitLiteral for NanoTime {
    fn emit_literal(&self) -> String {
        format!("::wingfoil::NanoTime::from({}u64)", u64::from(*self))
    }
}

impl<T: EmitLiteral + ?Sized> EmitLiteral for &T {
    fn emit_literal(&self) -> String {
        (**self).emit_literal()
    }
}

impl<T: EmitLiteral> EmitLiteral for Option<T> {
    fn emit_literal(&self) -> String {
        match self {
            Some(v) => format!("::core::option::Option::Some({})", v.emit_literal()),
            None => "::core::option::Option::None".to_string(),
        }
    }
}

impl<T: EmitLiteral> EmitLiteral for Vec<T> {
    fn emit_literal(&self) -> String {
        let items: Vec<String> = self.iter().map(EmitLiteral::emit_literal).collect();
        format!("::std::vec![{}]", items.join(", "))
    }
}

impl<T: EmitLiteral> EmitLiteral for [T] {
    fn emit_literal(&self) -> String {
        let items: Vec<String> = self.iter().map(EmitLiteral::emit_literal).collect();
        format!("[{}]", items.join(", "))
    }
}

/// Tuples, which is how an op with several data configs presents them
/// (`logged`'s `(String, Level)` shape).
macro_rules! emit_tuple {
    ($( ($($n:tt $t:ident),+) ),* $(,)?) => {$(
        impl<$($t: EmitLiteral),+> EmitLiteral for ($($t,)+) {
            fn emit_literal(&self) -> String {
                let parts: Vec<String> = ::std::vec![$(self.$n.emit_literal()),+];
                format!("({})", parts.join(", "))
            }
        }
    )*};
}

emit_tuple! {
    (0 A),
    (0 A, 1 B),
    (0 A, 1 B, 2 C),
    (0 A, 1 B, 2 C, 3 D),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contract, stated once: rendered source must evaluate back to an
    /// equal value. Each case below is checked by *writing out* the expected
    /// source, so a reader can see it is valid Rust — the round-trip itself is
    /// then proven by `reparse` tests in `tests/emit_literal.rs`, which compile
    /// the emitted text.
    #[test]
    fn integers_carry_a_type_suffix() {
        assert_eq!("1u64", 1u64.emit_literal());
        assert_eq!("-3i32", (-3i32).emit_literal());
        assert_eq!("0usize", 0usize.emit_literal());
    }

    #[test]
    fn floats_round_trip_and_name_the_non_finite() {
        assert_eq!("0.1f64", 0.1f64.emit_literal());
        assert_eq!("1e300f64", 1e300f64.emit_literal());
        assert_eq!("::core::primitive::f64::NAN", f64::NAN.emit_literal());
        assert_eq!(
            "::core::primitive::f64::INFINITY",
            f64::INFINITY.emit_literal()
        );
        assert_eq!(
            "::core::primitive::f64::NEG_INFINITY",
            f64::NEG_INFINITY.emit_literal()
        );
        // The classic: `Display` would print 0.30000000000000004 too, but
        // `Debug` is the one *specified* to round-trip.
        assert_eq!("0.30000000000000004f64", (0.1f64 + 0.2).emit_literal());
    }

    #[test]
    fn strings_are_escaped_by_rusts_own_rules() {
        assert_eq!(r#""hi""#, "hi".emit_literal());
        assert_eq!(r#""a\"b""#, "a\"b".emit_literal());
        assert_eq!(r#""tab\there""#, "tab\there".emit_literal());
        assert_eq!(r#""s".to_string()"#, "s".to_string().emit_literal());
    }

    #[test]
    fn duration_is_exact_not_unit_rounded() {
        assert_eq!(
            "::core::time::Duration::new(0u64, 1000000u32)",
            Duration::from_millis(1).emit_literal()
        );
        // A duration that no `from_<unit>` constructor round-trips.
        assert_eq!(
            "::core::time::Duration::new(2u64, 500000001u32)",
            Duration::new(2, 500_000_001).emit_literal()
        );
    }

    #[test]
    fn containers_nest() {
        assert_eq!(
            "::std::vec![1u8, 2u8]",
            ::std::vec![1u8, 2u8].emit_literal()
        );
        assert_eq!(
            "::core::option::Option::Some(true)",
            Some(true).emit_literal()
        );
        assert_eq!("::core::option::Option::None", None::<u8>.emit_literal());
        assert_eq!("(1u8, \"x\")", (1u8, "x").emit_literal());
        assert_eq!(
            "::std::vec![::core::option::Option::Some(1u8)]",
            ::std::vec![Some(1u8)].emit_literal()
        );
    }
}

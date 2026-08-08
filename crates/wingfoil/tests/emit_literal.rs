//! **Round-trip proof for [`EmitLiteral`]**: the source a value renders must
//! evaluate back to that value.
//!
//! The unit tests beside the trait assert what the *string* is. That is only
//! half the contract — a string can be exactly what you expected and still not
//! be valid Rust, or be valid Rust that evaluates to something else. So each
//! test here pairs the assertion with the **same text written out as real
//! source**. The file compiling proves the output parses; the equality proves
//! it reconstructs. Neither alone would.
//!
//! It is the same device `tests/emission_spike.rs` uses for whole wiring
//! functions, applied to single values.

use std::time::Duration;

use wingfoil::NanoTime;
use wingfoil::emit::EmitLiteral;

/// Assert that `$value` renders as `$literal`, *and* that `$literal` — written
/// here as real Rust — evaluates back to `$value`.
macro_rules! round_trips {
    ($value:expr, $literal:expr) => {{
        let original = $value;
        assert_eq!(
            $literal.to_string(),
            original.emit_literal(),
            "rendered source differs from the literal this test compiles"
        );
        original
    }};
}

#[test]
fn integers_round_trip_with_their_suffix() {
    let v = round_trips!(1u64, "1u64");
    assert_eq!(v, 1u64);

    let v = round_trips!(-3i32, "-3i32");
    assert_eq!(v, -3i32);

    let v = round_trips!(u128::MAX, "340282366920938463463374607431768211455u128");
    assert_eq!(v, 340282366920938463463374607431768211455u128);
}

#[test]
fn floats_round_trip_including_the_awkward_ones() {
    let v = round_trips!(0.1f64, "0.1f64");
    assert_eq!(v, 0.1f64);

    // The value `Display` formatting is famous for mangling.
    let v = round_trips!(0.1f64 + 0.2, "0.30000000000000004f64");
    assert_eq!(v, 0.30000000000000004f64);

    let v = round_trips!(f64::MIN_POSITIVE, "2.2250738585072014e-308f64");
    assert_eq!(v, 2.2250738585072014e-308f64);

    // Non-finite values have no literal form, so they are named instead.
    assert_eq!(
        "::core::primitive::f64::INFINITY",
        f64::INFINITY.emit_literal()
    );
    assert_eq!(f64::INFINITY, ::core::primitive::f64::INFINITY);
    assert!(f64::NAN.emit_literal().ends_with("::NAN"));
    assert!(::core::primitive::f64::NAN.is_nan());
}

#[test]
fn strings_round_trip_through_rusts_own_escaping() {
    let v = round_trips!("hi", r#""hi""#);
    assert_eq!(v, "hi");

    let v = round_trips!("a\"b\\c", r#""a\"b\\c""#);
    assert_eq!(v, "a\"b\\c");

    let v = round_trips!("tab\there\nnewline", r#""tab\there\nnewline""#);
    assert_eq!(v, "tab\there\nnewline");

    let v = round_trips!("owned".to_string(), r#""owned".to_string()"#);
    assert_eq!(v, "owned".to_string());
}

#[test]
fn duration_round_trips_exactly() {
    let v = round_trips!(
        Duration::from_millis(1),
        "::core::time::Duration::new(0u64, 1000000u32)"
    );
    assert_eq!(v, ::core::time::Duration::new(0u64, 1000000u32));

    // A value no `from_<unit>` constructor reconstructs — which is why the impl
    // uses `new(secs, nanos)`.
    let v = round_trips!(
        Duration::new(2, 500_000_001),
        "::core::time::Duration::new(2u64, 500000001u32)"
    );
    assert_eq!(v, ::core::time::Duration::new(2u64, 500000001u32));
}

#[test]
fn nanotime_round_trips() {
    let v = round_trips!(
        NanoTime::from(1_500_000u64),
        "::wingfoil::NanoTime::from(1500000u64)"
    );
    assert_eq!(v, ::wingfoil::NanoTime::from(1500000u64));
}

#[test]
fn containers_round_trip_and_nest() {
    let v = round_trips!(vec![1u8, 2, 3], "::std::vec![1u8, 2u8, 3u8]");
    assert_eq!(v, ::std::vec![1u8, 2u8, 3u8]);

    let v = round_trips!(Some(7u32), "::core::option::Option::Some(7u32)");
    assert_eq!(v, ::core::option::Option::Some(7u32));

    let v = round_trips!(None::<u32>, "::core::option::Option::None");
    assert_eq!(v, ::core::option::Option::None);

    let v = round_trips!((1u8, true), "(1u8, true)");
    assert_eq!(v, (1u8, true));

    let v = round_trips!(
        vec![Some(1u8), None],
        "::std::vec![::core::option::Option::Some(1u8), ::core::option::Option::None]"
    );
    assert_eq!(
        v,
        ::std::vec![
            ::core::option::Option::Some(1u8),
            ::core::option::Option::None
        ]
    );
}

/// Absolute paths throughout: a generated artifact is compiled in a scope the
/// generator does not control, so an impl relying on `use` statements being
/// present at the far end would produce source that compiles here and fails
/// there. This is the cheap guard against that regressing.
#[test]
fn rendered_paths_are_absolute() {
    for rendered in [
        Duration::from_secs(1).emit_literal(),
        NanoTime::from(0u64).emit_literal(),
        vec![1u8].emit_literal(),
        Some(1u8).emit_literal(),
        None::<u8>.emit_literal(),
        f64::INFINITY.emit_literal(),
    ] {
        let path = rendered
            .split(['(', '!', '[', ' '])
            .next()
            .expect("non-empty rendering");
        assert!(
            path.starts_with("::"),
            "path is not absolute in {rendered:?}"
        );
    }
}

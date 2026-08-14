//! Compile-fail tests pinning the `nitro!` macro's key error paths. Without
//! these, a refactor could silently start accepting (and mis-compiling) inputs
//! the macro is meant to reject. Run with `TRYBUILD=overwrite` to regenerate
//! the expected `.stderr` after an intentional message change.
//!
//! `tests/trybuild/pass/` is the other direction, and it is here rather than
//! in an ordinary integration test for a reason trybuild is uniquely good for:
//! it compiles each file as **its own crate, in its own throwaway Cargo
//! project**, with `wingfoil` as a plain path dependency. That is the only
//! arrangement in this repo where `crate::` cannot possibly reach the engine,
//! which is what makes it a real test of out-of-crate `#[op]` (#782) rather
//! than of `extern crate self as wingfoil`. `pass` runs the binary too, so the
//! fixture asserts its own results.
//!
//! Both live in one `#[test]`: a `TestCases` builds one shared project, and
//! two of them in parallel would race over it.

#[test]
fn nitro_macro_compile_failures() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/trybuild/*.rs");
    t.pass("tests/trybuild/pass/*.rs");
}

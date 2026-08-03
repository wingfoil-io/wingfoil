//! Compile-fail tests pinning the `nitro!` macro's key error paths. Without
//! these, a refactor could silently start accepting (and mis-compiling) inputs
//! the macro is meant to reject. Run with `TRYBUILD=overwrite` to regenerate
//! the expected `.stderr` after an intentional message change.

#[test]
fn nitro_macro_compile_failures() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/trybuild/*.rs");
}

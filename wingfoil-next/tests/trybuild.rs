//! Compile-fail tests pinning the `graph!` macro's key error paths. Without
//! these, a refactor could silently start accepting (and mis-compiling) inputs
//! the macro is meant to reject. Run with `TRYBUILD=overwrite` to regenerate
//! the expected `.stderr` after an intentional message change.

#[test]
fn graph_macro_compile_failures() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/trybuild/*.rs");
}

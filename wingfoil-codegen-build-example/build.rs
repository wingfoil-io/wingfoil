// Generates the static runner for the graph in `src/wiring.rs` at build time
// (written to `$OUT_DIR/graph.rs`, pulled in by `src/main.rs` via `include!`).
// Cargo re-runs this whenever the wiring changes, so the generated schedule
// can never go stale.

include!("src/wiring.rs");

fn main() -> wingfoil::codegen::Result<()> {
    println!("cargo:rerun-if-changed=src/wiring.rs");
    let (roots, _) = wire();
    wingfoil::codegen::emit_to_out_dir("graph.rs", roots, &Default::default())?;
    Ok(())
}

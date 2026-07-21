// Generates static runners at build time:
// - `$OUT_DIR/graph.rs` for the app graph in `src/wiring.rs` (see main.rs)
// - runner sets for the benchmark graphs in `bench_support/wiring.rs`, one
//   per engine tier (inline / dispatch / standalone), used by
//   `benches/tiers.rs` to compare tiers against the interpreted engine.
//
// Cargo re-runs this whenever either wiring file changes, so the generated
// schedules can never go stale.

use wingfoil::codegen::{CodegenOptions, emit_to_out_dir, generate_standalone};

include!("src/wiring.rs");

#[path = "bench_support/wiring.rs"]
mod bench_wiring;

/// Chain lengths for the dense scaling benchmark.
const CHAIN_LENS: &[usize] = &[16, 128, 1024];
/// Chain lengths for the sparse (quiet-floor) benchmark.
const SPARSE_LENS: &[usize] = &[128, 1024];

fn out_path(name: &str) -> std::path::PathBuf {
    let out_dir = std::env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo for build scripts");
    std::path::Path::new(&out_dir).join(name)
}

fn main() -> wingfoil::codegen::Result<()> {
    println!("cargo:rerun-if-changed=src/wiring.rs");
    println!("cargo:rerun-if-changed=bench_support/wiring.rs");

    // App runner (the build-time codegen example itself).
    let (roots, _) = wire();
    emit_to_out_dir("graph.rs", roots, &Default::default())?;

    let inline = CodegenOptions::default();
    let dispatch = CodegenOptions {
        inline: false,
        ..Default::default()
    };

    // Benchmark runners: odds/evens, all tiers.
    let (roots, _) = bench_wiring::wire_odds_evens();
    emit_to_out_dir("oe_inline.rs", roots, &inline)?;
    let (roots, _) = bench_wiring::wire_odds_evens();
    emit_to_out_dir("oe_dispatch.rs", roots, &dispatch)?;
    let (roots, _) = bench_wiring::wire_odds_evens();
    std::fs::write(
        out_path("oe_standalone.rs"),
        generate_standalone(roots, "run")?,
    )?;

    // The NxN fan-out graph (mirrors the interpreted-engine `10x10` bench),
    // all tiers.
    let (roots, _) = bench_wiring::wire_fanout(10, 10);
    emit_to_out_dir("fanout_10x10_inline.rs", roots, &inline)?;
    let (roots, _) = bench_wiring::wire_fanout(10, 10);
    emit_to_out_dir("fanout_10x10_dispatch.rs", roots, &dispatch)?;
    let (roots, _) = bench_wiring::wire_fanout(10, 10);
    std::fs::write(
        out_path("fanout_10x10_standalone.rs"),
        generate_standalone(roots, "run")?,
    )?;

    // Dense chains at several sizes, all tiers.
    for &len in CHAIN_LENS {
        let (roots, _) = bench_wiring::wire_chain(len);
        emit_to_out_dir(&format!("chain_inline_{len}.rs"), roots, &inline)?;
        let (roots, _) = bench_wiring::wire_chain(len);
        emit_to_out_dir(&format!("chain_dispatch_{len}.rs"), roots, &dispatch)?;
        let (roots, _) = bench_wiring::wire_chain(len);
        std::fs::write(
            out_path(&format!("chain_standalone_{len}.rs")),
            generate_standalone(roots, "run")?,
        )?;
    }

    // Sparse graphs (quiet chain + fast counter), schedule tiers only — the
    // interesting comparison is the quiet-node floor vs the interpreted
    // engine's dirty-lists.
    for &len in SPARSE_LENS {
        let roots = bench_wiring::wire_sparse(len);
        emit_to_out_dir(&format!("sparse_inline_{len}.rs"), roots, &inline)?;
        let roots = bench_wiring::wire_sparse(len);
        emit_to_out_dir(&format!("sparse_dispatch_{len}.rs"), roots, &dispatch)?;
    }
    Ok(())
}

//! Two-pass codegen: generate a compiled graph by *running* the wiring.
//!
//! `nitro!` reads tokens, so it cannot express a topology decided at run time —
//! a loop over N instruments discovered from a config file is out of reach. This
//! example runs that loop against the interpreted builder, walks the graph it
//! produced, and prints the N unrolled pipelines as `nitro!` input. The macro
//! never sees a loop.
//!
//! Both passes are visible here: `desk` below is pass 1, and `desk.gen.rs` — the
//! artifact it produced, checked in beside this file — is `include!`d, so pass 2
//! happened when you built this binary. The two run side by side at the end.
//!
//! Run `--regenerate` after changing the config or the wiring; the plain run
//! then verifies the checked-in file is current and fails loudly if it is not.

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode, codegen, wiring};

/// Where the artifact lives. Resolved at compile time so the example can be run
/// from anywhere in the tree.
const ARTIFACT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/core/codegen/desk.gen.rs"
);

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const CYCLES: RunFor = RunFor::Cycles(8);

/// One instrument's parameters. In a real desk these come from a file, a
/// database or a service — the point is that **nothing here is known when the
/// code is compiled**.
struct Instrument {
    name: &'static str,
    period: Duration,
    fee: f64,
    size: u64,
}

fn book() -> Vec<Instrument> {
    vec![
        Instrument {
            name: "ESZ5",
            period: Duration::from_millis(1),
            fee: 0.25,
            size: 50,
        },
        Instrument {
            name: "NQZ5",
            period: Duration::from_millis(4),
            fee: 1.75,
            size: 20,
        },
    ]
}

// -----------------------------------------------------------------------------
// Pass 1: the wiring. Ordinary Rust, with `#[wiring]` on it.
// -----------------------------------------------------------------------------

/// Note what is *not* here: no `func!`, no `_q` twin, no `with_src`, no
/// `with_cfg`, and no capture declaration. `#[wiring]` records every closure it
/// sees and finds `fee` and `size` by free-variable analysis.
#[wiring]
fn desk(g: &GraphBuilder, book: &[Instrument]) -> Stream<f64> {
    book.iter()
        .map(|inst| {
            let fee = inst.fee;
            let size = inst.size;

            g.ticker(inst.period)
                .count()
                .map(move |n: &u64| (n * size) as f64)
                .map(move |notional: &f64| notional - fee)
        })
        .reduce(|a, b| a.join(&b, |x: &f64, y: &f64| x + y))
        .expect("the book has at least one instrument")
}

// -----------------------------------------------------------------------------
// Pass 2: the artifact, compiled into this binary.
// -----------------------------------------------------------------------------

include!("desk.gen.rs");

fn main() -> anyhow::Result<()> {
    let book = book();

    println!("Config, read at run time — the compiler never saw any of it:");
    for i in &book {
        println!(
            "  {:<6} period {:>5?}   fee {:>5}   size {:>3}",
            i.name, i.period, i.fee, i.size
        );
    }

    // Pass 1: run the wiring and walk the graph it built.
    let generated = codegen::generate("desk_generated", "f64", |g| desk(g, &book))?;

    if std::env::args().any(|a| a == "--regenerate") {
        codegen::write_artifact(ARTIFACT, &generated)?;
        println!("\nRewrote {ARTIFACT}");
        return Ok(());
    }

    println!("\nPass 1 — running the wiring emits this `nitro!` input:\n");
    for line in generated.lines() {
        println!("  {line}");
    }

    // The loop is gone: each instrument became its own unrolled pipeline,
    // carrying the parameters it was configured with.
    println!(
        "\n  {} instruments -> {} tickers, 0 loops in the artifact.",
        book.len(),
        generated.matches("g.ticker(").count()
    );

    // The guard against the failure with no runtime symptom: an artifact built
    // from last quarter's fees compiles, runs, and looks plausible.
    codegen::check_artifact(ARTIFACT, &generated)?;
    println!("  The checked-in artifact is current.");

    // Pass 2: `desk.gen.rs` was compiled into this binary, so both tiers of it
    // are callable right here.
    println!("\nPass 2 — the artifact, compiled into this binary:\n");
    let interpreted = {
        let g = GraphBuilder::new();
        let out = desk(&g, &book).accumulate();
        let mut runner = g.build();
        runner.run(HISTORICAL, CYCLES)?;
        runner.value(out)
    };
    let (compiled,) = desk_generated::compiled(HISTORICAL, CYCLES)?;

    println!("  interpreted wiring : {interpreted:?}");
    println!("  compiled artifact  : {compiled:?} (final value)");
    assert_eq!(interpreted.last(), Some(&compiled));
    println!("\n  They agree — the artifact computes what the wiring computed.");

    // The other half of the contract: a graph the generator cannot emit is a
    // refusal naming the node, never a partial artifact.
    println!("\nWhat a refusal looks like:\n");
    let refused = codegen::generate("stateful", "u64", |g| {
        let journal = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        stateful(g, journal)
    })
    .expect_err("an unrenderable capture cannot be emitted");
    for line in refused.to_string().lines() {
        println!("  {line}");
    }

    Ok(())
}

/// A closure capturing something no artifact could reconstruct. It **wires and
/// runs** perfectly well — only generating from it is refused, which is why
/// capture detection renders values softly rather than demanding an
/// `EmitLiteral` bound on everything it finds.
#[wiring]
fn stateful(g: &GraphBuilder, journal: std::sync::Arc<std::sync::Mutex<Vec<u64>>>) -> Stream<u64> {
    g.ticker(Duration::from_millis(1))
        .count()
        .map(move |n: &u64| {
            journal.lock().expect("journal mutex poisoned").push(*n);
            *n
        })
}

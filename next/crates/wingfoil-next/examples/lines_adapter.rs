//! The dependency-free line-oriented file adapter end to end: replay a text
//! file through a graph, transform it, and write the result to another file —
//! the smallest complete Op-pattern I/O edge in both directions.
//!
//! ```sh
//! cargo run -p wingfoil-next --example lines_adapter
//! ```

use std::fs;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::Burst;
use wingfoil_next::adapters::lines::{LinesSinkOps, replay_lines};
use wingfoil_next::prelude::*;

fn main() -> anyhow::Result<()> {
    // A couple of temp files in the OS temp dir, uniquely named.
    let dir = std::env::temp_dir();
    let input = dir.join(format!("wingfoil_lines_in_{}.txt", std::process::id()));
    let output = dir.join(format!("wingfoil_lines_out_{}.txt", std::process::id()));
    fs::write(&input, "alpha\nbravo\ncharlie\ndelta\n")?;

    // Wire the graph: replay the file (deterministic historical replay, one
    // record per successive graph instant), upper-case each record, and write
    // the results out as lines.
    let g = GraphBuilder::new();
    let lines = replay_lines(&g, &input)?;
    let shouted = lines.map(|burst: &Burst<String>| {
        burst
            .iter()
            .map(|s| s.to_uppercase())
            .collect::<Burst<String>>()
    });
    let _sink = shouted.write_lines(&output)?;
    // Keep the original timestamps around to show the replay schedule.
    let stamped = lines.with_time().accumulate();

    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;

    println!("replayed records at their graph timestamps:");
    for (time, burst) in runner.value(&stamped) {
        println!("  {time}: {:?}", burst.iter().collect::<Vec<_>>());
    }

    println!("\nwrote {}:", output.display());
    for line in fs::read_to_string(&output)?.lines() {
        println!("  {line}");
    }

    fs::remove_file(&input).ok();
    fs::remove_file(&output).ok();
    Ok(())
}

//! The line-oriented file adapter end to end: replay a text file through a
//! graph, transform it, and write the result to another file — the smallest
//! complete Op-pattern I/O edge in both directions.
//!
//! The lazy historical replay source is behind the `async` feature (like
//! `csv_read`), so this example requires it:
//!
//! ```sh
//! cargo run -p wingfoil --example lines_adapter --features async
//! ```

use std::fs;

use wingfoil::Burst;
use wingfoil::adapters::lines::{LinesSinkOps, replay_lines};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

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
    let lines = replay_lines(&g, &input, None)?;
    let shouted = lines.map(|burst: &Burst<String>| {
        burst
            .iter()
            .map(|s| s.to_uppercase())
            .collect::<Burst<String>>()
    });
    let _sink = shouted.write_lines(&output)?;
    // Show the replay schedule as it happens: a second sink off the same
    // source, printing each record at the graph time it lands on.
    let _stamped = lines
        .with_time()
        .for_each(|(time, burst): &(NanoTime, Burst<String>)| {
            println!("  {time}: {:?}", burst.iter().collect::<Vec<_>>());
            Ok(())
        });

    let mut runner = g.build();
    println!("replayed records at their graph timestamps:");
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;

    println!("\nwrote {}:", output.display());
    for line in fs::read_to_string(&output)?.lines() {
        println!("  {line}");
    }

    fs::remove_file(&input).ok();
    fs::remove_file(&output).ok();
    Ok(())
}

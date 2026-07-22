//! The CSV adapter end to end: replay a CSV file as a deterministic historical
//! burst stream, transform each row, and write the result back to a CSV file.
//! Run with the `csv` feature:
//!
//! ```sh
//! cargo run -p wingfoil-next --features csv --example csv_adapter
//! ```

use std::io::Write;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::adapters::csv::{CsvSinkOps, csv_read};
use wingfoil_next::prelude::*;

/// A named-field record — serde field names become the CSV header columns.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Quote {
    timestamp: u64,
    price: f64,
}

fn main() -> anyhow::Result<()> {
    // Stage an input CSV in a temp directory (no header rows).
    let dir = std::env::temp_dir();
    let input = dir.join("wingfoil_next_csv_adapter_in.csv");
    let output = dir.join("wingfoil_next_csv_adapter_out.csv");
    {
        let mut f = std::fs::File::create(&input)?;
        writeln!(f, "100,10.0")?;
        writeln!(f, "200,11.5")?;
        writeln!(f, "300,9.75")?;
    }

    // Read → transform (bump every price by 1.0) → write, on the graph clock.
    let g = GraphBuilder::new();
    let rows = csv_read(&g, &input, |q: &Quote| NanoTime::new(q.timestamp), false)?;
    let bumped = rows.map(|b| {
        b.iter()
            .map(|q| Quote {
                timestamp: q.timestamp,
                price: q.price + 1.0,
            })
            .collect::<Burst<Quote>>()
    });
    let _sink = bumped.csv_write(&output)?;

    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;

    println!("wrote {}:", output.display());
    print!("{}", std::fs::read_to_string(&output)?);

    // Single-value convenience: a plain `Stream<T>` sinks by wrapping each value
    // into a one-element burst first.
    let g = GraphBuilder::new();
    let totals = csv_read(&g, &input, |q: &Quote| NanoTime::new(q.timestamp), false)?
        .map(|b| Burst::from([b.iter().map(|q| q.price).sum::<f64>()]));
    let _totals_sink = totals.csv_write(dir.join("wingfoil_next_csv_adapter_totals.csv"))?;
    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;

    Ok(())
}

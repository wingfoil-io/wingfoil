//! The polars adapter end to end: replay a Parquet file of quotes as a
//! deterministic historical burst stream, derive a mid price per row, and
//! collect the result back into a `DataFrame` and an Arrow IPC file. Run with
//! the `polars` feature:
//!
//! ```sh
//! cargo run -p wingfoil --features polars --example polars_adapter
//! ```

use std::sync::Arc;

use wingfoil::adapters::polars::polars::prelude::{Column, IntoColumn, NamedFrom, Series};
use wingfoil::adapters::polars::{
    AnyValue, DataFrame, DataType, PolarsFormat, PolarsRow, PolarsSinkOps, Schema, TimeUnit,
    polars_read,
};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// A named field of a row, or an error naming the missing column.
fn field<'r>(row: &'r PolarsRow, name: &str) -> anyhow::Result<&'r AnyValue<'static>> {
    row.get(name)
        .ok_or_else(|| anyhow::anyhow!("quote has no '{name}' column"))
}

fn main() -> anyhow::Result<()> {
    // Stage an input Parquet file: a microsecond `Datetime` time column, with
    // two quotes sharing the second instant.
    let dir = std::env::temp_dir();
    let input = dir.join("wingfoil_polars_adapter_quotes.parquet");
    let output = dir.join("wingfoil_polars_adapter_mids.arrow");
    let time = Series::new("time".into(), [1_i64, 2, 2, 3])
        .cast(&DataType::Datetime(TimeUnit::Microseconds, None))?
        .into_column();
    let mut quotes = DataFrame::new_infer_height(vec![
        time,
        Column::new("sym".into(), ["AAPL", "AAPL", "MSFT", "AAPL"]),
        Column::new("bid".into(), [100.0_f64, 100.5, 250.0, 101.0]),
        Column::new("ask".into(), [100.2_f64, 100.7, 250.4, 101.4]),
    ])?;
    PolarsFormat::Parquet.write(&input, &mut quotes)?;

    // Read → mid price → collect + write, on the graph clock.
    let g = GraphBuilder::new();
    let quotes = polars_read(&g, &input, "time", None)?;

    let schema = Arc::new(Schema::from_iter([
        ("sym".into(), DataType::String),
        ("mid".into(), DataType::Float64),
    ]));
    let mids = quotes.try_map(move |burst: &Burst<PolarsRow>| {
        burst
            .iter()
            .map(|q| {
                let bid: f64 = field(q, "bid")?.try_extract()?;
                let ask: f64 = field(q, "ask")?.try_extract()?;
                let sym = field(q, "sym")?.clone();
                PolarsRow::new(
                    schema.clone(),
                    vec![sym, AnyValue::Float64((bid + ask) / 2.0)],
                )
            })
            .collect::<anyhow::Result<Burst<PolarsRow>>>()
    });

    let _printed = mids.with_time().for_each(|(time, burst)| {
        let rows: Vec<String> = burst
            .iter()
            .map(|r| format!("{} {}", r.values()[0], r.values()[1]))
            .collect();
        println!("t={:>5}ns  {}", u64::from(*time), rows.join(" | "));
        Ok(())
    });
    let (_collect, collected) = mids.polars_collect();
    let _write = mids.polars_write(&output)?;

    let mut runner = g.build();
    runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;

    let frame = collected
        .frame()
        .ok_or_else(|| anyhow::anyhow!("the run did not complete"))?;
    let columns: Vec<String> = frame
        .schema()
        .iter()
        .map(|(name, dtype)| format!("{name}: {dtype}"))
        .collect();
    println!(
        "\ncollected {} rows [{}]",
        frame.height(),
        columns.join(", ")
    );
    let written = PolarsFormat::Ipc.read(&output)?;
    println!("wrote {} rows to {}", written.height(), output.display());
    Ok(())
}

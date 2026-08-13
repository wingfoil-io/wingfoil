#![doc = include_str!("./README.md")]

use std::time::Duration;

use anyhow::Result;
use wingfoil::adapters::postgres::{
    PostgresConnection, PostgresDeserialize, PostgresRowExt, PostgresSerialize, PostgresSinkOps,
    Row, ToSql, postgres_read, postgres_timestamp,
};
use wingfoil::async_source::RunParams;
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

const HOUR_NANOS: i64 = 3_600_000_000_000;
const TABLE: &str = "example_trades";

#[derive(Debug, Clone, Default, PartialEq)]
struct Trade {
    sym: String,
    price: f64,
    qty: i64,
}

impl PostgresDeserialize for Trade {
    fn from_row(row: &Row) -> Result<(NanoTime, Self)> {
        Ok((
            row.get_nanotime(0)?, // col 0: time
            Trade {
                sym: row.try_get(1)?,
                price: row.try_get(2)?,
                qty: row.try_get(3)?,
            },
        ))
    }
}

impl PostgresSerialize for Trade {
    fn to_params(&self) -> Vec<Box<dyn ToSql + Sync + Send>> {
        vec![
            Box::new(self.sym.clone()),
            Box::new(self.price),
            Box::new(self.qty),
        ]
    }
}

/// Deterministic sample trades, one per hour from the KDB epoch (2000-01-01).
fn trades(n: i64) -> Vec<(NanoTime, Trade)> {
    let syms = ["AAPL", "GOOG", "MSFT"];
    (0..n)
        .map(|i| {
            (
                NanoTime::from_kdb_timestamp(i * HOUR_NANOS),
                Trade {
                    sym: syms[i as usize % syms.len()].to_string(),
                    price: 100.0 + i as f64,
                    qty: i * 10 + 1,
                },
            )
        })
        .collect()
}

/// Create a fresh `example_trades` table (dropping any prior run's data).
fn reset_table(conn: &PostgresConnection) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let (client, connection) =
            tokio_postgres::connect(&conn.conn_str, tokio_postgres::NoTls).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
            .batch_execute(
                "DROP TABLE IF EXISTS example_trades; \
                 CREATE TABLE example_trades \
                   (time timestamp, sym text, price float8, qty int8)",
            )
            .await?;
        Ok::<(), anyhow::Error>(())
    })
}

fn main() -> Result<()> {
    let conn = PostgresConnection::new(
        "host=localhost port=5432 user=postgres password=postgres dbname=postgres",
    );
    let num_rows = 10;
    let start = NanoTime::from_kdb_timestamp(0);
    // The graph owns the tokio runtime (created lazily); each graph is built,
    // run, and dropped on this (non-async) thread so the reader/sink `block_on`s
    // are safe.

    reset_table(&conn)?;

    let baseline = trades(num_rows);

    // Write — replay the generated trades at their timestamps into the table. The
    // sink flushes at teardown, so scope the runner before reading back.
    {
        let g = GraphBuilder::new();
        let rows = baseline
            .iter()
            .cloned()
            .map(|(time, trade)| Ok((trade, time)));
        let _sink = g
            .replay_results(rows)
            .postgres_write(conn.clone(), TABLE, None)?;
        let mut runner = g.build();
        runner.run(
            RunMode::HistoricalFrom(start),
            RunFor::Duration(Duration::from_secs(num_rows as u64 * 3600 + 1)),
        )?;
    }

    // Read — time-sliced, one query per day (a single 24h slice covers the run).
    let g = GraphBuilder::new();
    let params = RunParams {
        run_mode: RunMode::HistoricalFrom(start),
        run_for: RunFor::Duration(Duration::from_secs(86400)),
        start_time: start,
    };
    let read = postgres_read::<Trade>(
        &g,
        params,
        conn,
        Duration::from_secs(86400),
        |(t0, t1), _date, _iter| {
            format!(
                "SELECT time, sym, price, qty FROM {TABLE} \
                 WHERE time >= '{}' AND time < '{}' ORDER BY time",
                postgres_timestamp(t0),
                postgres_timestamp(t1),
            )
        },
        None,
    )?
    // `accumulate` because the tie-out below compares the *whole* read-back
    // against the whole baseline — the assertion case it exists for, over a
    // bounded historical run. A graph that consumes rows rather than checking
    // them writes them out from a `for_each` sink instead.
    .collapse()
    .accumulate();
    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(start),
        RunFor::Duration(Duration::from_secs(86400)),
    )?;

    // Tie-out: the read-back trades must equal the generated ones.
    let read: Vec<Trade> = runner.value(&read);
    let expected: Vec<Trade> = baseline.into_iter().map(|(_, trade)| trade).collect();
    assert_eq!(read, expected, "generated and read data did not tie out");

    println!("✓ {num_rows} written, read and validated");
    Ok(())
}

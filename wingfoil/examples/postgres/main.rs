#![doc = include_str!("./README.md")]

use anyhow::Result;
use ordered_float::OrderedFloat;
use std::rc::Rc;
use wingfoil::adapters::postgres::*;
use wingfoil::*;

type Price = OrderedFloat<f64>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Trade {
    sym: String,
    price: Price,
    qty: i64,
}

impl PostgresDeserialize for Trade {
    fn from_row(row: &Row) -> Result<(NanoTime, Self)> {
        Ok((
            row.get_nanotime(0)?, // col 0: time
            Trade {
                sym: row.try_get(1)?,
                price: OrderedFloat(row.try_get(2)?),
                qty: row.try_get(3)?,
            },
        ))
    }
}

impl PostgresSerialize for Trade {
    fn to_params(&self) -> Vec<Box<dyn ToSql + Sync + Send>> {
        vec![
            Box::new(self.sym.clone()),
            Box::new(self.price.into_inner()),
            Box::new(self.qty),
        ]
    }
}

fn generate(num_rows: u32) -> Rc<dyn Stream<Burst<Trade>>> {
    let syms = ["AAPL", "GOOG", "MSFT"];
    ticker(std::time::Duration::from_secs(1))
        .count()
        .map(move |i| {
            burst![Trade {
                sym: syms[i as usize % syms.len()].to_string(),
                price: OrderedFloat(100.0 + i as f64),
                qty: (i * 10 + 1) as i64,
            }]
        })
        .limit(num_rows)
}

fn validate<T: Element + Eq>(a: Rc<dyn Stream<T>>, b: Rc<dyn Stream<T>>) -> Vec<Rc<dyn Node>> {
    fn assert_equal<T: Element + Eq>(a: Rc<dyn Stream<T>>, b: Rc<dyn Stream<T>>) -> Rc<dyn Node> {
        bimap(Dep::Active(a), Dep::Active(b), |a, b| {
            assert!(a == b, "generated and read data did not tie out");
        })
    }
    vec![
        assert_equal(a.clone(), b.clone()),
        assert_equal(a.count(), b.count()),
    ]
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
    env_logger::init();
    let conn = PostgresConnection::new(
        "host=localhost port=5432 user=postgres password=postgres dbname=postgres",
    );
    let table = "example_trades";
    let num_rows = 10;
    let run_mode = RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0));
    let run_for = RunFor::Duration(std::time::Duration::from_secs(11));

    reset_table(&conn)?;

    // write
    generate(num_rows)
        .postgres_write(conn.clone(), table)
        .run(run_mode, run_for)?;

    let baseline = generate(num_rows);
    // read — time-sliced, one query per day here (single 24h slice covers the run)
    let read = postgres_read::<Trade>(
        conn,
        std::time::Duration::from_secs(86400),
        move |(t0, t1), _date, _iter| {
            format!(
                "SELECT time, sym, price, qty FROM {table} \
                 WHERE time >= '{}' AND time < '{}' ORDER BY time",
                postgres_timestamp(t0),
                postgres_timestamp(t1),
            )
        },
    );

    // tie-out
    let check = validate(baseline, read);
    Graph::new(check, run_mode, run_for).run()?;
    println!("✓ {num_rows} written, read and validated");
    Ok(())
}

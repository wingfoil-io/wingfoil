# PostgreSQL Adapter Example

Round-trips data through PostgreSQL: generates trades, writes them with
`postgres_write`, reads them back with the time-sliced `postgres_read`, and asserts
the two streams tie out. Demonstrates the on-graph time model (`(NanoTime, T)` tuples)
and the shared time-slicing logic used for historical replay.

## Setup

```sh
docker run --rm -p 5432:5432 -e POSTGRES_PASSWORD=postgres postgres:16-alpine
```

## Run

```sh
cargo run --example postgres --features postgres
```

## Code

```rust
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

fn main() -> Result<()> {
    let conn = PostgresConnection::new(
        "host=localhost port=5432 user=postgres password=postgres dbname=postgres",
    );
    let run_mode = RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0));
    let run_for = RunFor::Duration(std::time::Duration::from_secs(11));

    // ... create the table, then:
    generate(10)
        .postgres_write(conn.clone(), "example_trades")
        .run(run_mode, run_for)?;

    let read = postgres_read::<Trade>(
        conn,
        std::time::Duration::from_secs(86400),
        move |(t0, t1), _date, _iter| {
            format!(
                "SELECT time, sym, price, qty FROM example_trades \
                 WHERE time >= '{}' AND time < '{}' ORDER BY time",
                postgres_timestamp(t0),
                postgres_timestamp(t1),
            )
        },
    );
    // ... run and tie out
    Ok(())
}
```

See [`main.rs`](./main.rs) for the full listing.

## Output

```
✓ 10 written, read and validated
```

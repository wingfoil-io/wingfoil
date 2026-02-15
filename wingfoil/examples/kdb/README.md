# KDB+ Adapter Example

This example demonstrates the KDB+ adapter by:
1. Generating mock trade data
2. Writing it to KDB+
3. Reading it back from KDB+
4. Validating that the read data matches the generated data

## Setup

Start a KDB+ instance on port 5000:

```sh
q -p 5000
```

Then in the q console, create the test_trades table:

```q
test_trades:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())
```

## Run

```sh
cargo run --example kdb
```

To delete the records and reset the example:

```q
delete from `test_trades
```

## Query Details

The query used is:
```q
`time xasc select from test_trades
```

The explicit sorting is only needed if the data is written unordered.
For this example, we could have used:
```q
select from test_trades
```

## Code

```rust
use anyhow::Result;
use ordered_float::OrderedFloat;
use std::rc::Rc;
use wingfoil::adapters::kdb::*;
use wingfoil::*;

type Price = OrderedFloat<f64>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Trade {
    sym: Sym,
    price: Price,
    qty: i64,
}

impl KdbDeserialize for Trade {
    fn from_kdb_row(
        row: Row<'_>,
        _columns: &[String],
        interner: &mut SymbolInterner,
    ) -> Result<Self, KdbError> {
        Ok(Trade {
            sym: row.get_sym(1, interner)?,
            price: OrderedFloat(row.get(2)?.get_float()?),
            qty: row.get(3)?.get_long()?,
        })
    }
}

impl KdbSerialize for Trade {
    fn to_kdb_row(&self) -> K {
        K::new_compound_list(vec![
            K::new_symbol(self.sym.to_string()),
            K::new_float(self.price.into_inner()),
            K::new_long(self.qty),
        ])
    }
}

fn generate(num_rows: u32) -> Rc<dyn Stream<Burst<Trade>>> {
    let syms = ["AAPL", "GOOG", "MSFT"];
    let mut interner = SymbolInterner::default();
    let syms_interned: Vec<Sym> = syms.iter().map(|s| interner.intern(s)).collect();
    ticker(std::time::Duration::from_nanos(1_000_000_000))
        .count()
        .map(move |i| {
            burst![Trade {
                sym: syms_interned[i as usize % syms_interned.len()].clone(),
                price: OrderedFloat(100.0 + i as f64),
                qty: (i * 10 + 1) as i64,
            }]
        })
        .limit(num_rows)
}

fn main() -> Result<()> {
    env_logger::init();
    let conn = KdbConnection::new("localhost", 5000);
    let table = "test_trades";
    let time_col = "time";
    let query = format!("`time xasc select from {table}");
    let chunk = 10000;
    let num_rows = 10;
    let run_mode = RunMode::HistoricalFrom(NanoTime::ZERO);
    let run_for = RunFor::Forever;
    // Write
    generate(num_rows)
        .kdb_write(conn.clone(), table)
        .run(run_mode, run_for)?;
    let baseline = generate(num_rows);
    // Read
    let read = kdb_read(conn, query, time_col, chunk);
    // Validate
    let check = validate(baseline, read);
    Graph::new(check, run_mode, run_for).run()?;
    println!("✓ {num_rows} written, read and validated");
    Ok(())
}
```

## Output

```
✓ 10 written, read and validated
```

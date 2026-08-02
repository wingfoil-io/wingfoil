# Fluvio Adapter Example (wingfoil-next)

This example demonstrates using the next Fluvio adapter to seed records into a
topic, consume them in a graph, apply a transformation (uppercase), and write the
results to a second topic — all from a single `GraphBuilder`.

A port of the classic `wingfoil/examples/fluvio` example onto the next engine.

## Setup

Start a local Fluvio cluster and create the required topics:

```sh
# Start cluster (requires the Fluvio CLI)
fluvio cluster start --local

# Create topics
fluvio topic create fluvio-example-source
fluvio topic create fluvio-example-dest
```

## Run

```sh
cargo run -p wingfoil-next --example fluvio_adapter --features fluvio
```

## Code

```rust
use wingfoil_next::{RunFor, RunMode};
use wingfoil_next::adapters::fluvio::{FluvioEvent, FluvioRecord, FluvioSinkOps, fluvio_sub};
use wingfoil_next::prelude::*;

const ENDPOINT: &str = "127.0.0.1:9003";
const SOURCE_TOPIC: &str = "fluvio-example-source";
const DEST_TOPIC: &str = "fluvio-example-dest";

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let g = GraphBuilder::new().with_async_runtime(rt.handle().clone());

    let _seed = g
        .constant(burst![
            FluvioRecord::with_key("greeting", b"hello".to_vec()),
            FluvioRecord::with_key("subject", b"world".to_vec()),
        ])
        .fluvio_pub(ENDPOINT, SOURCE_TOPIC, None)?;

    let _transform = fluvio_sub(&g, RunMode::RealTime, ENDPOINT, SOURCE_TOPIC, 0, None)?
        .map(|burst: &Burst<FluvioEvent>| {
            burst
                .iter()
                .map(|event| {
                    let key = event.key_str().and_then(|r| r.ok()).unwrap_or("").to_string();
                    let upper = event.value_str().unwrap_or("").to_uppercase().into_bytes();
                    println!("  {} → {}", key, String::from_utf8_lossy(&upper));
                    FluvioRecord::with_key(key, upper)
                })
                .collect::<Burst<FluvioRecord>>()
        })
        .fluvio_pub(ENDPOINT, DEST_TOPIC, None)?;

    g.build().run(RunMode::RealTime, RunFor::Cycles(3))?;
    Ok(())
}
```

## Output

```
  greeting → HELLO
  subject → WORLD
```

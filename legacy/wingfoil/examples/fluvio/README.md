# Fluvio Adapter Example

This example demonstrates using the Fluvio adapter to seed records into a topic,
consume them in a graph, apply a transformation (uppercase), and write the results
to a second topic — all within a single wingfoil `Graph`.

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
cargo run --example fluvio --features fluvio
```

## Code

```rust
use wingfoil::adapters::fluvio::*;
use wingfoil::*;

const ENDPOINT: &str = "127.0.0.1:9003";
const SOURCE_TOPIC: &str = "fluvio-example-source";
const DEST_TOPIC: &str = "fluvio-example-dest";

fn main() -> anyhow::Result<()> {
    let conn = FluvioConnection::new(ENDPOINT);

    let seed = constant(burst![
        FluvioRecord::with_key("greeting", b"hello".to_vec()),
        FluvioRecord::with_key("subject", b"world".to_vec()),
    ])
    .fluvio_pub(conn.clone(), SOURCE_TOPIC);

    let transform = fluvio_sub(conn.clone(), SOURCE_TOPIC, 0, None)
        .map(|burst| {
            burst
                .into_iter()
                .map(|event| {
                    let key = event.key_str().and_then(|r| r.ok()).unwrap_or("").to_string();
                    let upper = event.value_str().unwrap_or("").to_uppercase().into_bytes();
                    println!("  {} → {}", key, String::from_utf8_lossy(&upper));
                    FluvioRecord::with_key(key, upper)
                })
                .collect::<Burst<FluvioRecord>>()
        })
        .fluvio_pub(conn, DEST_TOPIC);

    Graph::new(vec![seed, transform], RunMode::RealTime, RunFor::Cycles(3)).run()?;
    Ok(())
}
```

## Output

```
  greeting → HELLO
  subject → WORLD
```

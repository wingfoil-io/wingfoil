//! zmq pub/sub adapter example — publish a counter, subscribe, print it.
//!
//! A self-contained port of the legacy `legacy/wingfoil/examples/zmq` (direct mode)
//! onto the next engine. Two roles run as separate graphs:
//!
//! - **publisher** — on a background thread, binds a `PUB` socket and publishes
//!   a UTF-8 counter every 100 ms.
//! - **subscriber** — in `main`, connects a `SUB` socket to that address and
//!   prints each received value (plus connection-status transitions).
//!
//! The publisher buffers early messages until the subscriber's subscription
//! propagates, so no counter values are dropped at startup (ZeroMQ's
//! slow-joiner problem). ZMQ is peer-to-peer — no broker process is required.
//! Service discovery via `EtcdRegistry` (`("name", registry)`) is exercised by
//! the `zmq-etcd-integration-test` suite.
//!
//! # Run
//!
//! ```sh
//! cargo run -p wingfoil-next --example zmq_adapter --features zmq
//! ```

use std::time::Duration;

use wingfoil_next::adapters::zmq::{ZeroMqPub, ZmqStatus, zmq_sub};
use wingfoil_next::prelude::*;
use wingfoil_next::{RunFor, RunMode};

const ADDRESS: &str = "tcp://127.0.0.1:5556";
const PORT: u16 = 5556;

fn main() -> anyhow::Result<()> {
    // Publisher: bind a PUB socket and publish a counter for two seconds.
    let publisher = std::thread::spawn(move || -> anyhow::Result<()> {
        let g = GraphBuilder::new();
        let _pub = g
            .ticker(Duration::from_millis(100))
            .count()
            .map(|n: &u64| format!("{n}").into_bytes())
            .zmq_pub(PORT, ());
        g.build()
            .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)))?;
        Ok(())
    });

    // Subscriber: connect and print received counters + status transitions.
    let g = GraphBuilder::new();
    let (data, status) = zmq_sub::<Vec<u8>>(&g, RunMode::RealTime, ADDRESS)?;
    let _print = data.collapse().for_each(|msg: &Vec<u8>| {
        println!("received: {}", String::from_utf8_lossy(msg));
        Ok(())
    });
    let _status = status.for_each(|s: &ZmqStatus| {
        println!("status: {s:?}");
        Ok(())
    });
    g.build().run(
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(1500)),
    )?;

    publisher.join().expect("publisher thread panicked")?;
    Ok(())
}

# Wingfoil Examples

A guide to the examples in this directory. Each one is runnable — see its own `README.md` for setup and commands.

## Core concepts

| Example | Description |
|---|---|
| [`order_book`](order_book/) | Load NASDAQ AAPL limit orders from CSV, maintain an order book, derive trades and two-way prices, export to CSV. |
| [`breadth_first`](breadth_first/) | Why wingfoil's BFS execution avoids the O(2^N) node explosion of naive depth-first DAGs. |
| [`run_mode`](run_mode/) | Swap `RunMode::RealTime` and `RunMode::HistoricalFrom` with the same graph wiring for backtesting. |
| [`async`](async/) | Integrate Tokio async/await at graph edges (I/O adapters) while keeping the core graph synchronous. |
| [`threading`](threading/) | Distribute graph execution across worker threads with `producer()` / `mapper()`. |
| [`dynamic`](dynamic/) | Add and remove nodes at runtime. Includes `demux` (static slot pool), `dynamic-group` (high-level API), `dynamic-manual` (low-level `MutableNode`). |
| [`tracing`](tracing/) | Instrumentation modes (log, tracing, instruments) for event and span handling. |
| [`latency`](latency/) | Per-hop latency stamping with `Traced<T, L>` and `LatencyReport`, transported over iceoryx2. |

## I/O adapters

| Example | Description |
|---|---|
| [`kdb`](kdb/) | KDB+ integration: [`read`](kdb/read/) (time-sliced queries), [`read_cached`](kdb/read_cached/) (with LRU file cache), [`round_trip`](kdb/round_trip/) (write + read + validate). |
| [`kafka`](kafka/) | Kafka / Redpanda adapter — subscribe, transform, publish pipeline via `rdkafka`. |
| [`fluvio`](fluvio/) | Fluvio distributed streaming — subscribe, transform, publish pipeline. |
| [`fix`](fix/) | FIX 4.4 protocol: [`fix_loopback`](fix/fix_loopback.rs) (self-contained), [`fix_client`](fix/fix_client.rs), [`fix_echo_server`](fix/fix_echo_server.rs), [`lmax_demo`](fix/lmax_demo.rs) (live LMAX market data over TLS), [`lmax_instruments`](fix/lmax_instruments.rs). |
| [`zmq`](zmq/) | ZeroMQ pub/sub: [`direct`](zmq/direct/) (direct addressing) and [`etcd`](zmq/etcd/) (service discovery via etcd). |
| [`etcd`](etcd/) | etcd key-value store adapter for sub/pub with transformation. |
| [`iceoryx2`](iceoryx2/) | Zero-copy IPC over shared memory (spin, threaded, signaled polling modes). |
| [`web`](web/) | WebSocket adapter streaming synthetic prices and receiving UI events. |
| [`telemetry`](telemetry/) | Metrics export: [`prometheus`](telemetry/prometheus/) (pull-based scrape) and [`otlp`](telemetry/otlp/) (push to Grafana Alloy, Datadog, Honeycomb, etc.). |

## Snippets

The rest of this file shows short snippets for the adapter examples. For the full, runnable code see each example's own directory.

### KDB+

Define a typed struct, implement `KdbDeserialize` to map rows, and stream time-sliced queries directly into your graph:

```rust,ignore
#[derive(Debug, Clone, Default)]
struct Price {
    sym: Sym,
    mid: f64,
}

kdb_read::<Price, _>(
    conn,
    Duration::from_secs(10),
    |(t0, t1), _date, _iter| {
        format!(
            "select time, sym, mid from prices \
             where time >= (`timestamp$){}j, time < (`timestamp$){}j",
            t0.to_kdb_timestamp(),
            t1.to_kdb_timestamp(),
        )
    },
)
.logged("prices", Info)
.run(
    RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
    RunFor::Duration(Duration::from_secs(100)),
)?;
```

[Full example.](kdb/read/)

### etcd

Watch a key prefix, transform values, and write results back — all in a declarative graph:

```rust,ignore
use wingfoil::adapters::etcd::*;
use wingfoil::*;

let conn = EtcdConnection::new("http://localhost:2379");

let round_trip = etcd_sub(conn.clone(), "/source/")
    .map(|burst| {
        burst.into_iter().map(|event| {
            let upper = event.entry.value_str().unwrap_or("").to_uppercase().into_bytes();
            EtcdEntry { key: event.entry.key.replacen("/source/", "/dest/", 1), value: upper }
        })
        .collect::<Burst<EtcdEntry>>()
    })
    .etcd_pub(conn, None, true);

round_trip.run(RunMode::RealTime, RunFor::Cycles(3)).unwrap();
```

[Full example.](etcd/)

### Fluvio

Stream records from a Fluvio topic, transform them, and write results back — leveraging Fluvio's distributed streaming platform for durable pub/sub:

```rust,ignore
use wingfoil::adapters::fluvio::*;
use wingfoil::*;

let conn = FluvioConnection::new("127.0.0.1:9003");

let round_trip = fluvio_sub(conn.clone(), "source", 0, None)
    .map(|burst| {
        burst.into_iter().map(|event| {
            let upper = String::from_utf8_lossy(&event.value).to_uppercase();
            FluvioRecord::with_key(event.key_str().unwrap_or(""), upper.into_bytes())
        })
        .collect::<Burst<FluvioRecord>>()
    })
    .fluvio_pub(conn, "dest");

round_trip.run(RunMode::RealTime, RunFor::Cycles(10)).unwrap();
```

[Full example.](fluvio/)

### Kafka

Consume messages from a Kafka topic, transform them, and produce the results to another topic — backed by `rdkafka` with in-burst pipelined writes:

```rust,ignore
use wingfoil::adapters::kafka::*;
use wingfoil::*;

let conn = KafkaConnection::new("localhost:9092");

let round_trip = kafka_sub(conn.clone(), "source", "example-group")
    .map(|burst| {
        burst.into_iter().map(|event| {
            let upper = event.value_str().unwrap_or("").to_uppercase().into_bytes();
            KafkaRecord { topic: "dest".into(), key: event.key, value: upper }
        })
        .collect::<Burst<KafkaRecord>>()
    })
    .kafka_pub(conn);

round_trip.run(RunMode::RealTime, RunFor::Cycles(10)).unwrap();
```

[Full example.](kafka/)

### ZeroMQ

Publish a stream over ZMQ and subscribe from another process — cross-language compatible with the Python bindings:

```rust,ignore
// publisher
use wingfoil::adapters::zmq::ZeroMqPub;
use wingfoil::*;

ticker(Duration::from_millis(100))
    .count()
    .map(|n: u64| format!("{n}").into_bytes())
    .zmq_pub(7779, ())
    .run(RunMode::RealTime, RunFor::Forever)?;
```

```rust,ignore
// subscriber
use wingfoil::adapters::zmq::zmq_sub;
use wingfoil::*;

let (data, _status) = zmq_sub::<Vec<u8>>("tcp://127.0.0.1:7779")?;
// See wingfoil-python/examples/zmq/ for a Python subscriber
data.print()
    .run(RunMode::RealTime, RunFor::Forever)?;
```

Service discovery via etcd is also supported — see [`zmq/etcd`](zmq/etcd/) for details.

[Full example.](zmq/)

### FIX protocol

Connect to a FIX 4.4 exchange (e.g. LMAX London Demo) over TLS, subscribe to market data, and process incoming messages — all as a streaming graph:

```rust,ignore
use wingfoil::adapters::fix::fix_connect_tls;
use wingfoil::*;

let fix = fix_connect_tls(
    "fix-marketdata.london-demo.lmax.com", 443,
    &username, "LMXBDM", Some(&password),
);

// Subscribe to EUR/USD — waits for LoggedIn, then sends the request.
let sub = fix.fix_sub(constant(vec!["4001".into()]));

let data_node = fix.data.logged("fix-data", Info).as_node();
let status_node = fix.status.logged("fix-status", Info).as_node();

Graph::new(
    vec![data_node, status_node, sub],
    RunMode::RealTime,
    RunFor::Duration(Duration::from_secs(60)),
)
.run()
.unwrap();
```

Run the self-contained loopback example (no external FIX engine needed):

```sh
RUST_LOG=info cargo run --example fix_loopback --features fix
```

[Full examples.](fix/)

### Telemetry

Export stream metrics to Grafana via Prometheus scraping (pull) or OpenTelemetry OTLP push — or both simultaneously:

```rust,ignore
use wingfoil::adapters::prometheus::PrometheusExporter;
use wingfoil::adapters::otlp::{OtlpConfig, OtlpPush};
use wingfoil::*;

let exporter = PrometheusExporter::new("0.0.0.0:9091");
exporter.serve()?;

let config = OtlpConfig {
    endpoint: "http://localhost:4318".into(),
    service_name: "my-app".into(),
};

let counter = ticker(Duration::from_secs(1)).count();
let prometheus_node = exporter.register("wingfoil_ticks_total", counter.clone());
let otlp_node = counter.otlp_push("wingfoil_ticks_total", config);

Graph::new(vec![prometheus_node, otlp_node], RunMode::RealTime, RunFor::Forever).run()?;
```

[Full example.](telemetry/)

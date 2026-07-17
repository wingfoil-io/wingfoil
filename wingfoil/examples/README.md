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

## Adapters

| Example | Description |
|---|---|
| [`kdb`](kdb/) | KDB+ integration: [`read`](kdb/read/) (time-sliced queries), [`read_cached`](kdb/read_cached/) (with LRU file cache), [`round_trip`](kdb/round_trip/) (write + read + validate). |
| [`kafka`](kafka/) | Kafka / Redpanda adapter — subscribe, transform, publish pipeline via `rdkafka`. |
| [`fluvio`](fluvio/) | Fluvio distributed streaming — subscribe, transform, publish pipeline. |
| [`fix`](fix/) | FIX 4.4 protocol: [`fix_loopback`](fix/fix_loopback.rs) (self-contained), [`fix_client`](fix/fix_client.rs), [`fix_echo_server`](fix/fix_echo_server.rs), [`lmax_demo`](fix/lmax_demo.rs) (live LMAX market data over TLS), [`lmax_instruments`](fix/lmax_instruments.rs). |
| [`zmq`](zmq/) | ZeroMQ pub/sub: [`direct`](zmq/direct/) (direct addressing) and [`etcd`](zmq/etcd/) (service discovery via etcd). |
| [`etcd`](etcd/) | etcd key-value store adapter for sub/pub with transformation. |
| [`redis`](redis/) | Redis adapter — Pub/Sub channels (subscribe, transform, republish) and persistent Streams (snapshot + tail). |
| [`postgres`](postgres/) | PostgreSQL adapter — time-sliced historical reads and streaming writes, round-trip write/read/validate. |
| [`iceoryx2`](iceoryx2/) | Zero-copy IPC over shared memory (spin, threaded, signaled polling modes). |
| [`aeron`](aeron/) | Low-latency Aeron UDP/IPC transport — publish and subscribe to `i64` values with spin and threaded polling modes. |
| [`web`](web/) | WebSocket adapter streaming synthetic prices and receiving UI events. |
| [`telemetry`](telemetry/) | Metrics export: [`prometheus`](telemetry/prometheus/) (pull-based scrape) and [`otlp`](telemetry/otlp/) (push to Grafana Alloy, Datadog, Honeycomb, etc.). |
| [`augurs`](augurs/) | augurs time-series toolkit — on-graph forecasting (ETS/MSTL), outlier detection (MAD/DBSCAN), changepoint, seasonality, DTW and clustering over sliding windows. |
| [`statistics`](statistics/) | Streaming statistics toolkit — EWMA (per-tick and time-decayed), cumulative and rolling mean/variance/std/min/max/median, with count- and time-weighted variants over sample- and time-based windows. |

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

kdb_read::<Price>(
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

### Redis

Subscribe to a Redis Pub/Sub channel, uppercase each payload, and republish to another channel. Redis Pub/Sub is fire-and-forget, so subscribers must be live before a message is published:

```rust,ignore
use wingfoil::adapters::redis::*;
use wingfoil::*;

let conn = RedisConnection::new("redis://127.0.0.1:6379");

let processor = redis_sub(conn.clone(), "source")
    .map(|burst| {
        burst.into_iter().map(|event| {
            let upper = event.payload_str().unwrap_or("").to_uppercase().into_bytes();
            RedisEntry { channel: "dest".into(), payload: upper }
        })
        .collect::<Burst<RedisEntry>>()
    })
    .redis_pub(conn);

processor.run(RunMode::RealTime, RunFor::Forever).unwrap();
```

The adapter also supports Redis **Streams** for a persistent, replayable log:
`redis_stream_write` appends entries via `XADD`, and `redis_stream_read` replays
existing entries (`XRANGE`) before tailing live appends (`XREAD BLOCK`).

[Full example.](redis/)

### PostgreSQL

Replay a PostgreSQL table historically in time slices, then write records back. Time
is carried on-graph (`(NanoTime, T)`) — extracted from a `timestamp` column on read and
prepended on write. `postgres_read` shares its time-slicing logic with the KDB+ adapter:

```rust,ignore
use wingfoil::adapters::postgres::*;
use wingfoil::*;

#[derive(Debug, Clone, Default)]
struct Trade { sym: String, price: f64, qty: i64 }

impl PostgresDeserialize for Trade {
    fn from_row(row: &Row) -> anyhow::Result<(NanoTime, Self)> {
        Ok((
            row.get_nanotime(0)?, // col 0: time
            Trade { sym: row.try_get(1)?, price: row.try_get(2)?, qty: row.try_get(3)? },
        ))
    }
}

let conn = PostgresConnection::new("host=localhost user=postgres password=postgres dbname=postgres");

postgres_read::<Trade>(conn, std::time::Duration::from_secs(3600), |(t0, t1), _date, _| {
    format!(
        "SELECT time, sym, price, qty FROM trades \
         WHERE time >= '{}' AND time < '{}' ORDER BY time",
        postgres_timestamp(t0), postgres_timestamp(t1),
    )
})
    .collapse()
    .print()
    .run(
        RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
        RunFor::Duration(std::time::Duration::from_secs(86400)),
    )
    .unwrap();
```

[Full example.](postgres/)

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

### iceoryx2

Zero-copy IPC over shared memory between processes. The payload is a `#[repr(C)]` `ZeroCopySend` type; the subscriber picks a polling mode (`Spin`, `Threaded`, or `Signaled`) trading latency for CPU:

```rust,ignore
use iceoryx2::prelude::ZeroCopySend;
use wingfoil::adapters::iceoryx2::{Iceoryx2Mode, Iceoryx2SubOpts, iceoryx2_pub, iceoryx2_sub_opts};
use wingfoil::*;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend)]
struct Counter {
    seq: u64,
}

// publisher
let upstream = ticker(Duration::from_millis(100)).count().map(|seq: u64| burst![Counter { seq }]);
iceoryx2_pub(upstream, "wingfoil/examples/counter")
    .run(RunMode::RealTime, RunFor::Forever)?;
```

```rust,ignore
// subscriber (in another process)
let opts = Iceoryx2SubOpts { mode: Iceoryx2Mode::Spin, ..Default::default() };
iceoryx2_sub_opts::<Counter>("wingfoil/examples/counter", opts)
    .collapse()
    .inspect(|c: &Counter| println!("received seq={}", c.seq))
    .run(RunMode::RealTime, RunFor::Forever)?;
```

[Full example.](iceoryx2/)

### Aeron

Publish `i64` values over a low-latency Aeron channel and subscribe to them back. The subscriber polls Aeron directly inside the graph `cycle()` (spin mode) for zero thread-crossing latency:

```rust,ignore
use std::time::Duration;
use wingfoil::adapters::aeron::{
    AeronHandle, AeronPub, AeronSubOptions, FragmentBuffer, TransportError, aeron_sub_fragment,
};
use wingfoil::*;

let handle = AeronHandle::connect()?; // requires a running media driver
let sub = handle.subscription("aeron:ipc", 1001, Duration::from_secs(5))?;
let pub_ = handle.publication("aeron:ipc", 1001, Duration::from_secs(5))?;

let received = aeron_sub_fragment(
    sub,
    |f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        Ok(f.as_ref().try_into().ok().map(i64::from_le_bytes))
    },
    AeronSubOptions::default(),
);
let publisher = received.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

Graph::new(
    vec![received.print().as_node(), publisher],
    RunMode::RealTime,
    RunFor::Cycles(10),
)
.run()?;
```

Use `AeronMode::Threaded` to poll on a background thread instead. The pure-Rust
`aeron-rs` backend needs no C++ toolchain but takes a lock on the graph
thread — see the [adapter docs](../src/adapters/aeron/CLAUDE.md).

[Full example.](aeron/)

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

### Web

Stream values to one or more browsers over WebSocket and receive UI events back, all as on-graph streams. The `WebServer` hosts an HTTP + WebSocket listener on its own tokio runtime:

```rust,ignore
use wingfoil::adapters::web::*;
use wingfoil::*;

let server = WebServer::bind("127.0.0.1:0").start()?;
let port = server.port();
println!("open ws://127.0.0.1:{port}/ws");

// publish: graph → browser
ticker(Duration::from_millis(10))
    .count()
    .web_pub(&server, "tick")
    .run(RunMode::RealTime, RunFor::Forever)?;
```

```rust,ignore
// subscribe: browser → graph
let clicks: Rc<dyn Stream<Burst<u32>>> = web_sub(&server, "ui_events");
clicks.collapse().print().run(RunMode::RealTime, RunFor::Forever)?;
```

Serve a static UI bundle with `.serve_static("./dist")`, and enable the `web-tls` feature with `.tls(cert, key)` for HTTPS/WSS.

[Full example.](web/)

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

### augurs

On-graph time-series analysis with the [augurs](https://docs.rs/augurs) toolkit — no external service. Six windowed operators: `augurs_forecast` (ETS or MSTL), `augurs_outlier` (MAD or DBSCAN), `augurs_changepoint`, `augurs_seasons`, `augurs_dtw` and `augurs_cluster`:

```rust,ignore
use wingfoil::adapters::augurs::*;
use wingfoil::*;

// Forecast a noisy upward ramp 5 steps ahead with 90% prediction intervals
// (add `.mstl(vec![24])` to the config for a seasonal MSTL model instead).
ticker(Duration::from_secs(1))
    .count()
    .map(|n| n as f64 + (n as f64 * 0.5).sin())
    .augurs_forecast(AugursForecastConfig::new(48, 5).with_level(0.90))
    .for_each(|f, _| println!("next 5: {:?}", f.point))
    .run(RunMode::RealTime, RunFor::Forever)?;

// Flag the series that diverges from the group (one Vec<f64> per tick).
readings // Rc<dyn Stream<Vec<f64>>>
    .augurs_outlier(AugursOutlierConfig::mad(40, 0.5))
    .for_each(|o, _| println!("outlying: {:?}", o.outlying))
    .run(RunMode::RealTime, RunFor::Forever)?;

// Detect regime changes and seasonal periods in a single series.
prices.augurs_changepoint(AugursChangepointConfig::new(120));
prices.augurs_seasons(AugursSeasonsConfig::new(240));
```

[Full example.](augurs/)

### Statistics

Streaming numeric aggregations via the `StatisticsOperators` trait — no external service, but an explicit import (it lives under `adapters`, not the prelude). Every aggregate takes a `Window` — `Count(n)` (last `n` samples), `Time(duration)` (last `duration` of graph time), or `Unbounded` (cumulative) — so one method serves both rolling and cumulative; the moment operators also take a `Weighting` (`Count` = every sample equal, `Time` = weighted by how long each value was in effect):

```rust,ignore
use wingfoil::*;
use wingfoil::adapters::statistics::*;

// Exponential smoothing: per-tick alpha, or a time-based half-life.
prices.ewma(EwmaSpan::PerTick(0.2));
prices.ewma(EwmaSpan::HalfLife(Duration::from_secs(30)));

// Cumulative mean — arithmetic vs time-weighted (TWAP).
prices.mean(Window::Unbounded, Weighting::Count);
prices.mean(Window::Unbounded, Weighting::Time);

// Rolling over the last N samples, or the last N of graph time.
prices.std(Window::Count(20), Weighting::Count);
prices.median(Window::Count(20), Weighting::Time);
prices.mean(Window::Time(Duration::from_secs(5)), Weighting::Time);
```

[Full example.](statistics/)

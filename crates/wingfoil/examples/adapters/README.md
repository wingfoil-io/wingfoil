# Adapter Examples

One directory per I/O adapter — the edges where a graph meets the outside world.
Each has its own README with prerequisites, the wiring, and expected output.

Every adapter is **feature-gated**, so nothing you don't ask for is compiled or
linked.

## No server required

Start here. These four run with nothing installed.

| Adapter | Feature | Run | What it does |
|---|---|---|---|
| [`lines`](lines/) | `async` | `--example lines_adapter` | The smallest complete I/O edge: replay a text file, transform, write it out. |
| [`csv`](csv/) | `csv` | `--example csv_adapter` | Typed rows with real event timestamps; deterministic historical replay. |
| [`augurs`](augurs/) | `augurs` | `--example augurs_adapter` | On-graph forecasting, outlier / changepoint / season detection, DTW, clustering. |
| [`zmq`](zmq/) | `zmq` | `--example zmq_adapter` | Brokerless pub/sub — publisher and subscriber in one process. |

## Message buses and brokers

| Adapter | Feature | Run | Needs |
|---|---|---|---|
| [`kafka`](kafka/) | `kafka` | `--example kafka_adapter` | A Kafka/Redpanda broker |
| [`fluvio`](fluvio/) | `fluvio` | `--example fluvio_adapter` | A Fluvio cluster (`fluvio_admin` bootstraps one without the CLI) |
| [`redis`](redis/) | `redis` | `--example redis_adapter` | Redis (Pub/Sub) |
| [`etcd`](etcd/) | `etcd` | `--example etcd_adapter` | etcd |

## Low-latency transports

| Adapter | Feature | Run | What it does |
|---|---|---|---|
| [`iceoryx2`](iceoryx2/) | `iceoryx2` | `--example iceoryx2_sub` / `iceoryx2_pub` | Zero-copy IPC over shared memory. Run the subscriber first. |
| [`aeron`](aeron/) | `aeron` | `--example aeron_adapter` | Aeron UDP/IPC. Also `aeron_status_circuit_breaker` — a breaker driven off the status stream. |

## Stores

| Adapter | Feature | Run | What it does |
|---|---|---|---|
| [`kdb`](kdb/) | `kdb` | `--example kdb_read` / `kdb_read_cached` / `kdb_round_trip` | Time-sliced reads, an LRU file cache, and a write/read/validate loop. |
| [`postgres`](postgres/) | `postgres` | `--example postgres_adapter` | Time-sliced historical reads and streaming writes. |

## Protocols and the web

| Adapter | Feature | Run | What it does |
|---|---|---|---|
| [`fix`](fix/) | `fix` | `--example fix_adapter` | FIX 4.4 loopback — acceptor and initiator in one process, no external engine. |
| [`web`](web/) | `web` | `--example web_adapter` | WebSocket **server**: stream prices to a browser, receive UI events back. |
| [`ws`](ws/) | `ws` | `--example ws_adapter` | WebSocket **client**: survives a venue hanging up, re-subscribing on every reconnect. |

## Market data

| Adapter | Feature | Run | What it does |
|---|---|---|---|
| [`market`](market/) | `market,fix,kdb` | `--example market_adapter` | One strategy over the venue-neutral book vocabulary, fed by either impl of its feed trait: LMAX FIX (realtime) or a kdb+ replay (historical). |

## Telemetry

| Adapter | Feature | Run | What it does |
|---|---|---|---|
| [`prometheus`](prometheus/) | `prometheus` | `--example prometheus_adapter` | Serve `/metrics` for scraping (pull). |
| [`otlp`](otlp/) | `otlp,prometheus` | `--example otlp_adapter` | Push over OTLP *and* serve `/metrics` (push + pull). |
| [`telemetry`](telemetry/) | — | `run.sh` | The shared Docker harness (Prometheus, Grafana, Alloy) both of the above scrape into. |

## Running an adapter example

The feature is always required:

```sh
cargo run -p wingfoil --example <name> --features <feature>
```

Note that the target *name* and the directory name differ for most adapters — the
directory is named after the adapter (`csv/`), the target keeps its historical
name (`csv_adapter`). The tables above give both.

## How an adapter is put together

Three conventions run through all of them:

- **Sources emit `Burst<T>`.** Everything that arrived at the same graph instant
  is delivered together — never coalesced, never latest-wins, never dropped. A
  single value still arrives as a burst of one.
- **Sinks are graph roots.** `csv_write`, `redis_pub`, `prometheus_gauge` and
  friends return a handle you keep alive; the write is driven by the graph, on the
  graph's clock, not by a background task doing its own thing.
- **Async adapters let the graph own the tokio runtime.** It is created lazily on
  first use, so no `&Handle` is threaded through your wiring. Pass one explicitly
  with `GraphBuilder::new().with_async_runtime(handle)` only when the runtime must
  outlive or predate the graph — [`etcd`](etcd/) is the example that does.

Several adapters also expose connection state as a **stream** alongside the data
(`zmq_sub` and `fix_connect` both return a `(data, status)` pair), so reconnects
are ordinary graph events you can fold or gate on rather than callbacks.

To add a new adapter, follow the `/new-adapter` skill — it carries the
source/sink shapes, feature gating, the parity obligation against the classic
tree, and the test tiers.

## Elsewhere

- [`../core/`](../core/) — engine concepts, no services needed.
- [`../showcase/`](../showcase/) — several adapters composed into one end-to-end demo.
- [`../../src/adapters/`](../../src/adapters/) — the adapters themselves; each has its own `CLAUDE.md`.

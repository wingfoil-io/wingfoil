# 🚀 Wingfoil

[![PyPI - Version](https://img.shields.io/pypi/v/wingfoil.svg)](https://pypi.org/project/wingfoil/)
[![Documentation Status](https://readthedocs.org/projects/wingfoil/badge/?version=latest)](https://wingfoil.readthedocs.io/en/latest/)
[![CI](https://github.com/wingfoil-io/wingfoil/actions/workflows/rust-test.yml/badge.svg)](https://github.com/wingfoil-io/wingfoil/actions/workflows/rust-test.yml)

Wingfoil is a **blazingly fast**, highly scalable
[stream processing framework](https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/benches/)
designed for **latency-critical** use cases such as electronic trading and real-time AI
systems. You define a graph of transformations over streams; Wingfoil drives their
execution in a tightly scheduled [DAG](https://en.wikipedia.org/wiki/Directed_acyclic_graph),
either against live data or replayed history.

The Rust engine does the heavy lifting; this `wingfoil` package gives you the same graph
model, operators, and production-ready I/O adapters from Python.

---

## Table of Contents

- [Features](#-features)
- [Installation](#-installation)
- [Quick Start](#-quick-start)
- [Core Concepts](#-core-concepts)
- [Stream Operators](#-stream-operators)
- [Composing Streams: `Graph`, `bimap`, `CustomStream`](#-composing-streams-graph-bimap-customstream)
- [Backtesting with Historical Mode](#-backtesting-with-historical-mode)
- [Pandas Integration](#-pandas-integration)
- [I/O Adapters](#-io-adapters)
  - [CSV](#csv)
  - [KDB+](#kdb)
  - [etcd](#etcd)
  - [ZeroMQ](#zeromq)
  - [iceoryx2 (shared memory)](#iceoryx2-shared-memory)
  - [FIX protocol](#fix-protocol)
  - [Prometheus](#prometheus)
  - [OpenTelemetry OTLP](#opentelemetry-otlp)
- [Build from Source](#-build-from-source)
- [Release Status & Feedback](#-release-status--feedback)

---

## ✨ Features

- **Fast** — ultra-low latency and high throughput with an efficient DAG execution
  engine written in Rust.
- **Simple and obvious** — define your graph with fluent operators; Wingfoil manages
  scheduling and data propagation.
- **Backtesting out of the box** — switch from real-time to historical replay by
  flipping a single flag.
- **Production I/O adapters** — CSV, KDB+, etcd, ZeroMQ, iceoryx2, FIX 4.4
  (incl. TLS), Prometheus, and OTLP — ready to plug into your graph.
- **Multi-language** — Rust crate and Python package today, WASM/JS/TS planned.

---

## 📦 Installation

```bash
pip install wingfoil
```

Wingfoil wheels are published for Linux, macOS, and Windows on CPython 3.8+.

Optional adapters require the matching server/library (KDB+, etcd, iceoryx2, a
FIX counterparty, OTLP collector, etc.) but no additional Python dependencies —
the adapter clients are compiled into the wheel.

---

## ⚡ Quick Start

```python
from wingfoil import ticker

(
    ticker(1.0)                       # tick every second
        .count()                      # 1, 2, 3, ...
        .map(lambda n: f"hello, world {n}")
        .logged(">>")                 # INFO-log each value
        .run(realtime=True, duration=3.0)
)
```

```
[INFO wingfoil] 0.000_092 >> hello, world 1
[INFO wingfoil] 1.008_038 >> hello, world 2
[INFO wingfoil] 2.012_219 >> hello, world 3
```

`run()` blocks until the stop condition is reached. Pass any of:

| Argument | Type | Meaning |
| --- | --- | --- |
| `realtime` | `bool` | `True` uses wall-clock time; `False` is historical replay. |
| `start` | `float` \| `datetime` | Historical start (Unix-seconds float or UTC `datetime`). |
| `duration` | `float` \| `timedelta` | Stop after this many seconds of graph time. |
| `cycles` | `int` | Stop after this many engine cycles. |

---

## 🧠 Core Concepts

- **Stream** — a time-stamped channel of values. Streams are produced by sources
  (`ticker`, `constant`, I/O adapters) and transformed with operators
  (`.map`, `.filter`, `.distinct`, ...). Every operator returns a new `Stream`.
- **Node** — anything schedulable. A `Stream` is a `Node` that also carries a value;
  pure side-effect sinks (`.for_each`, `.csv_write`, `.zmq_pub`, ...) return a `Node`.
- **Graph** — a bundle of roots that share one engine run. Use `Graph([...])` when
  you have several independent stream branches that must execute together
  (e.g. publisher + subscriber + monitoring).
- **Active vs. passive upstreams** — an active upstream *triggers* downstream
  execution on tick; a passive upstream is read but does not trigger. Most built-in
  operators use active inputs; `.sample(trigger)` is the common way to fire a
  stream from a different clock.

### Run Modes

- `realtime=True` — the engine tracks wall-clock time. Use with live inputs
  (sockets, brokers, iceoryx2, etc.).
- `realtime=False` — **historical replay**, driven by event timestamps. Ideal for
  backtests and deterministic tests. In historical mode the graph runs as fast
  as the CPU allows; time advances purely from source events.

---

## 🧰 Stream Operators

All methods are available on `Stream` instances. Examples assume
`from wingfoil import ticker, constant, bimap, Graph`.

### Source operators

| Operator | Description |
| --- | --- |
| `ticker(period)` | Emit once every `period` seconds. Returns a `Node`. |
| `constant(value)` | Emit `value` once on the first cycle. |

### Transforming values

| Operator | Description |
| --- | --- |
| `.map(f)` | Apply `f(value)` to each tick. |
| `.filter(pred)` | Drop values where `pred(value)` is false. |
| `.distinct()` | Drop consecutive duplicates. |
| `.difference()` | Emit `current - previous`. |
| `.delay(seconds)` | Replay values delayed by `seconds`. |
| `getattr(s, 'not')()` | Logical/arithmetic negation (the literal method name is `not`; invoke via `getattr` because `not` is a Python keyword). |
| `.limit(n)` | Pass through at most `n` values, then stop. |
| `.sample(trigger)` | Re-emit the current value on each `trigger` tick. |

### Aggregation

| Operator | Description |
| --- | --- |
| `.count()` | Emit tick count: 1, 2, 3, ... |
| `.sum()` | Running sum (values must be numeric). |
| `.average()` | Running mean (values must be numeric). |
| `.buffer(n)` | Tumbling window of size `n`. |
| `.collect()` | Accumulate every value into a `list` emitted each cycle. |
| `.with_time()` | Pair each value with graph-time as `(seconds, value)`. |
| `.dataframe()` | Collect `[(time, value), ...]` for pandas (see below). |

### Observing and sinking

| Operator | Description |
| --- | --- |
| `.inspect(f)` | Call `f(value)` and pass the value through. |
| `.logged("label")` | `INFO`-log each value and pass it through. |
| `.for_each(f)` | Terminal sink: `f(value, time)` on every tick. |
| `getattr(s, 'finally')(f)` | Terminal sink: `f(final_value)` called once at shutdown (literal method name `finally` collides with Python's keyword, so use `getattr`). |
| `.peek_value()` | After `run()`, inspect the last emitted value. |

### Execution

| Operator | Description |
| --- | --- |
| `.run(realtime, start=, duration=, cycles=)` | Build and run a one-root graph. |
| `Graph([...]).run(...)` | Build and run a multi-root graph. |

#### Example: most operators in one pipeline

```python
from wingfoil import ticker

avg_of_odds = (
    ticker(0.1)
        .count()
        .filter(lambda x: x % 2 == 1)   # 1, 3, 5, ...
        .map(float)
        .average()                      # running mean
        .logged("avg")
)

avg_of_odds.run(realtime=False, cycles=10)
print("last:", avg_of_odds.peek_value())
```

---

## 🧱 Composing Streams: `Graph`, `bimap`, `CustomStream`

### `Graph` — run several roots together

```python
from wingfoil import ticker, Graph

quotes = ticker(0.1).count().map(lambda i: 100 + i).logged("quote")
heartbeat = ticker(1.0).count().logged("heartbeat")

Graph([quotes, heartbeat]).run(realtime=True, duration=2.5)
```

### `bimap` — fuse two streams

```python
from wingfoil import ticker, constant, bimap

a = ticker(0.1).count()                       # 1, 2, 3, ...
b = constant(0.5).sample(ticker(0.1))         # 0.5 on every tick

(bimap(a, b, lambda x, y: x + y)
    .logged("sum")
    .run(realtime=False, cycles=5))
```

### `CustomStream` — write your own operator in Python

Subclass `CustomStream` and implement `cycle()`:

```python
import math
from wingfoil import ticker, CustomStream

class Polynomial(CustomStream):
    """Sum of upstream[i] * 10**i."""

    def cycle(self):
        value = sum(
            src.peek_value() * math.pow(10, i)
            for i, src in enumerate(self.upstreams())
        )
        self.set_value(value)
        return True

source = ticker(0.1).count()
(
    Polynomial([source] * 3)
        .map(lambda x: x * 0.01)
        .logged("poly")
        .run(realtime=False, cycles=5)
)
```

---

## 🕰️ Backtesting with Historical Mode

Pass `realtime=False` to drive the graph from source timestamps rather than the
wall clock. Add `start=` if your sources require a specific epoch start (such as
`kdb_read`), and cap the replay with `duration=` or `cycles=`.

```python
from datetime import datetime, timezone
from wingfoil import ticker

stream = ticker(0.01).count().collect()
stream.run(
    realtime=False,
    start=datetime(2025, 1, 1, tzinfo=timezone.utc),
    cycles=5,
)
print(stream.peek_value())   # [1, 2, 3, 4, 5]
```

Historical mode is deterministic — it's the right mode for unit tests and
strategy backtests.

---

## 🐼 Pandas Integration

`wingfoil` ships with two pandas helpers:

- `stream.dataframe()` — collects `(time, value)` pairs into a list; combine with
  `wingfoil.to_dataframe` to materialise a `pandas.DataFrame`.
- `wingfoil.build_dataframe({"col": stream, ...})` — aligns several
  `.dataframe()` streams by graph time.

```python
from wingfoil import ticker, Graph, build_dataframe

source = ticker(0.01).count().limit(5)
prices = source.map(lambda i: 100 + i).dataframe()
quantities = source.map(lambda _: 10).dataframe()

Graph([prices, quantities]).run(realtime=False)

df = build_dataframe({"price": prices, "qty": quantities})
print(df)
#       time  price  qty
# 0  0.0e+00    101   10
# 1  1.0e-02    102   10
# ...
```

A single-stream variant using `to_dataframe`:

```python
from wingfoil import ticker, to_dataframe

stream = (
    ticker(0.01)
        .count()
        .limit(5)
        .map(lambda i: {"price": 100 + i, "qty": 10})
        .dataframe()
)
stream.run(realtime=False)
df = to_dataframe(stream.peek_value())
print(df)
```

---

## 🔌 I/O Adapters

All adapters are exposed from the top-level `wingfoil` module. Every write
method (`csv_write`, `kdb_write`, `etcd_pub`, `zmq_pub`, `iceoryx2_pub`,
`otlp_push`) returns a `Node` — drive it by calling `.run(...)`.

### CSV

Read a CSV file into a stream of dicts (keys = column headers, values = strings).
The file must have a header row and a timestamp column encoded as integer
nanoseconds since the Unix epoch.

```python
from wingfoil import csv_read

rows = csv_read("prices.csv", time_column="time_ns").collect()
rows.run(realtime=False)
print(rows.peek_value())         # [{'time_ns': '...', 'sym': 'AAPL', ...}, ...]
```

Write a stream of dicts to CSV. Headers are inferred from the first dict; a
`time` column with graph-time nanoseconds is prepended automatically.

```python
from wingfoil import ticker

(
    ticker(0.1)
        .count()
        .limit(5)
        .map(lambda i: {"sym": "AAPL", "price": 100.0 + i})
        .csv_write("out.csv")
        .run(realtime=False)
)
```

### KDB+

Start a q process (`q -p 5000`) and create the target table:

```
test_trades:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())
```

```python
from wingfoil import ticker, kdb_read

HOST, PORT, TABLE = "localhost", 5000, "test_trades"

# Write: each dict becomes one row; "columns" names the non-time columns.
(
    ticker(1.0).count().limit(10)
        .map(lambda i: {"sym": "AAPL", "price": 100.0 + i, "qty": i * 10 + 1})
        .kdb_write(
            host=HOST, port=PORT, table=TABLE,
            columns=[("sym", "symbol"), ("price", "float"), ("qty", "long")],
        )
        .run(realtime=False)
)

# Read: time-sliced query; returns a stream of dicts.
# `start` and `duration` bound the replay window against the KDB time column.
rows = kdb_read(
    host=HOST, port=PORT,
    query=f"select from {TABLE}",
    time_col="time",
    chunk_size=10_000,
).collect()
rows.run(realtime=False, start=946684800.0, duration=86400.0)
print(rows.peek_value())
```

Supported `kdb_write` column types: `"symbol"`, `"float"`, `"long"`, `"int"`,
`"bool"`.

### etcd

Start etcd (`docker run --rm -p 2379:2379 gcr.io/etcd-development/etcd:v3.5.0`).

```python
from wingfoil import ticker, etcd_sub

ENDPOINT = "http://localhost:2379"

# Publish: each dict = {"key": str, "value": bytes}, or a list of them per tick.
(
    ticker(1.0).count().limit(3)
        .map(lambda i: {"key": f"/wf/item/{i}", "value": str(i).encode()})
        .etcd_pub(ENDPOINT, lease_ttl=30.0, force=True)
        .run(realtime=True)
)

# Subscribe: snapshot + watch events under a prefix; each tick = list[event].
events = etcd_sub(ENDPOINT, "/wf/").inspect(print)
events.run(realtime=True, duration=2.0)
```

Event dicts have shape:
`{"kind": "put"|"delete", "key": str, "value": bytes, "revision": int}`.

### ZeroMQ

Cross-language compatible — the Rust publisher/subscriber inter-operate with
Python on both sides.

**Direct mode** — hard-coded address, no discovery infrastructure:

```python
# zmq_pub.py
import wingfoil as wf

(
    wf.ticker(0.5).count()
        .map(lambda n: str(n).encode())
        .zmq_pub(port=7779)
        .run(realtime=True)
)
```

```python
# zmq_sub.py
import wingfoil as wf

data, status = wf.zmq_sub("tcp://127.0.0.1:7779")
data_node = data.inspect(lambda msgs: [print("msg:", m) for m in msgs])
status_node = status.inspect(lambda s: print("status:", s))
wf.Graph([data_node, status_node]).run(realtime=True)
```

`zmq_sub` returns `(data_stream, status_stream)`. Each `data_stream` tick yields
`list[bytes]` of messages received that cycle. `status_stream` yields
`"connected"` / `"disconnected"`.

**etcd discovery** — publishers register under a service name; subscribers look
it up. Useful for dynamic deployments. Requires a running etcd.

```python
# publisher
wf.ticker(0.5).count().map(lambda n: str(n).encode()) \
    .zmq_pub_etcd("quotes", 7779, "http://127.0.0.1:2379") \
    .run(realtime=True)

# subscriber
data, status = wf.zmq_sub_etcd("quotes", "http://127.0.0.1:2379")
```

For multi-host deployments where `127.0.0.1` isn't routable, use
`zmq_pub_etcd_on(name, address, port, endpoint)`.

### iceoryx2 (shared memory)

Zero-copy pub/sub over shared memory. Requires building wingfoil with the
`iceoryx2-beta` feature (opt-in; see [Build from Source](#-build-from-source)).

```python
from wingfoil import ticker, iceoryx2_sub, Iceoryx2ServiceVariant, Iceoryx2Mode, Graph

service = "wingfoil/demo"

sub = iceoryx2_sub(
    service,
    variant=Iceoryx2ServiceVariant.Local,   # or Ipc
    mode=Iceoryx2Mode.Signaled,             # Spin | Threaded | Signaled
)
sub = sub.inspect(lambda msgs: print("received:", msgs)).collect()

pub = (
    ticker(0.1).count()
        .map(lambda n: f"tick {n}".encode())
        .iceoryx2_pub(service, variant=Iceoryx2ServiceVariant.Local)
)

Graph([pub, sub]).run(realtime=True, duration=0.5)
```

Both ends accept `variant` (`Ipc` for cross-process, `Local` for same-process),
`history_size`, and publisher-side `initial_max_slice_len`.

### FIX protocol

FIX 4.4 initiator, TLS initiator, and acceptor. All return
`(data_stream, status_stream)`; TLS additionally returns a sender object
for sending outbound messages.

```python
from wingfoil import fix_connect

data, status = fix_connect(
    host="fix.example.com",
    port=9876,
    sender_comp_id="MYCOMP",
    target_comp_id="BROKER",
)

messages = data.inspect(lambda msgs: [print("fix:", m) for m in msgs])
states = status.inspect(lambda ss: [print("session:", s) for s in ss])

import wingfoil as wf
wf.Graph([messages, states]).run(realtime=True, duration=10.0)
```

Each `data` tick yields a `list[dict]` where every dict is
`{"msg_type": str, "seq_num": int, "fields": [(tag, value), ...]}`.
Status values are `"disconnected" | "logging_in" | "logged_in"` or a dict
`{"status": "logged_out"|"error", "reason"|"message": str}`.

TLS initiator (e.g. LMAX) with a sender:

```python
from wingfoil import fix_connect_tls

data, status, sender = fix_connect_tls(
    host="fix-marketdata.london-digital.lmax.com",
    port=443,
    sender_comp_id="USERNAME",
    target_comp_id="LMXBL",
    password="secret",
)

# Send a FIX message on the session:
sender.send({
    "msg_type": "V",
    "fields": [(262, "req1"), (263, "1"), (264, "0")],
})
```

Acceptor:

```python
from wingfoil import fix_accept

data, status = fix_accept(port=9876, sender_comp_id="MYCOMP", target_comp_id="INIT")
```

### Prometheus

Expose any stream as a gauge metric on a Prometheus-compatible `/metrics`
endpoint.

```python
from wingfoil import ticker, Graph, PrometheusExporter

exporter = PrometheusExporter("0.0.0.0:9091")
exporter.serve()                              # bind and start the HTTP server

tick_count = ticker(1.0).count()
requests_count = ticker(0.1).count()

Graph([
    exporter.register("tick_count", tick_count),
    exporter.register("requests_count", requests_count),
]).run(realtime=True, duration=5.0)
```

Scrape with `curl http://localhost:9091/metrics`.

### OpenTelemetry OTLP

Push any stream's value to an OTLP HTTP collector as a gauge metric.

```python
from wingfoil import ticker

(
    ticker(1.0).count()
        .otlp_push(
            metric_name="wingfoil_ticks",
            endpoint="http://localhost:4318",
            service_name="demo",
        )
        .run(realtime=True, duration=10.0)
)
```

---

## 🛠️ Build from Source

Most users should `pip install wingfoil`. To build locally (e.g. to enable the
`iceoryx2-beta` feature or develop against the bindings), see
[`build.md`](https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil-python/build.md).

```bash
git clone https://github.com/wingfoil-io/wingfoil
cd wingfoil/wingfoil-python
pip install maturin
maturin develop                               # or: maturin develop --features iceoryx2-beta
pytest
```

---

## 📢 Release Status & Feedback

The Wingfoil Python module is currently a **beta release**. APIs are stabilising
and we would love your input — especially if you:

- are interested in contributing,
- know of a project Wingfoil is a good fit for,
- want to request a feature, or
- have any feedback.

Email us at [hello@wingfoil.io](mailto:hello@wingfoil.io), open a
[GitHub discussion](https://github.com/wingfoil-io/wingfoil/discussions/), or
browse the [issue tracker](https://github.com/wingfoil-io/wingfoil/issues).

More resources:

- 📚 Python examples:
  <https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil-python/examples>
- 🦀 Rust crate docs: <https://docs.rs/wingfoil>

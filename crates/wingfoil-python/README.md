# wingfoil-python

[![PyPI - Version](https://img.shields.io/pypi/v/wingfoil?logo=pypi&logoColor=white)](https://pypi.org/project/wingfoil/)
[![PyPI - Python versions](https://img.shields.io/pypi/pyversions/wingfoil?logo=python&logoColor=white)](https://pypi.org/project/wingfoil/)
[![Python docs](https://img.shields.io/readthedocs/wingfoil/latest?logo=readthedocs&logoColor=white&label=python%20docs)](https://wingfoil.readthedocs.io/en/latest/)

Wingfoil is a blazingly fast, highly scalable stream processing framework
designed for latency-critical use cases: electronic trading, real-time
decisioning and streaming ML features. You define a graph of transformations
over streams; Wingfoil drives their execution in a tightly scheduled
[DAG](https://en.wikipedia.org/wiki/Directed_acyclic_graph), either against
live data or replayed history.

**Wingfoil** is the Op-pattern engine that replaces the original wingfoil
engine, and `wingfoil` is its Python binding. The Rust engine does the
heavy lifting; this package exposes the same graph model, the combinator
surface, all fifteen production I/O adapters and the latency-tracing surface —
plus a plugin seam that lets you author ops, sub-graphs and adapters *in Rust*
and compose them from Python.

> Coming from the original `wingfoil` package? This one supersedes it —
> see the [migration guide](https://github.com/wingfoil-io/wingfoil/blob/next/crates/wingfoil-python/docs/migration.rst), which lists every renamed
> entry point and every place wingfoil deliberately behaves differently.

---

## Table of Contents

- [Features](#features)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Core Concepts](#core-concepts)
  - [Run modes and the run window](#run-modes-and-the-run-window)
  - [Bursts: why a source tick is a list](#bursts-why-a-source-tick-is-a-list)
- [Stream Operators](#stream-operators)
  - [Sources](#sources)
  - [Transforming values](#transforming-values)
  - [Gating and rate control](#gating-and-rate-control)
  - [Combining and splitting](#combining-and-splitting)
  - [Aggregation](#aggregation)
  - [Observing and reading back](#observing-and-reading-back)
  - [Statistics](#statistics)
- [Python-defined nodes](#python-defined-nodes)
- [Backtesting with historical mode](#backtesting-with-historical-mode)
- [Pandas integration](#pandas-integration)
- [Latency tracing](#latency-tracing)
- [I/O adapters](#io-adapters)
- [Authoring components in Rust](#authoring-components-in-rust)
- [Build from source](#build-from-source)
- [Testing](#testing)
- [Documentation](#documentation)
- [Release status and feedback](#release-status-and-feedback)

---

## Features

- **Fast** — ultra-low latency and high throughput from an efficient DAG
  execution engine written in Rust. The Python edge boxes values only at the
  boundary; wiring is done once, not per tick.
- **Simple and obvious** — build a graph with fluent combinators; Wingfoil
  manages scheduling and propagation.
- **Backtesting out of the box** — the same graph runs against wall-clock time
  or replays history deterministically; historical is the *default*.
- **Lossless** — same-instant values ride a single burst, never coalesced to
  latest-wins and never dropped, identically in both run modes.
- **Fails loudly** — a missing field, a wrong-typed value or a raising callable
  aborts the run naming the offender, rather than defaulting its way through.
- **Fifteen production I/O adapters** — PostgreSQL, Kafka, Redis, etcd, Fluvio,
  CSV, KDB+, ZeroMQ, FIX 4.4, augurs, Prometheus, OTLP, WebSocket, Aeron and
  iceoryx2.
- **Latency tracing** — per-hop wall-clock stamps that survive a hop to a Rust
  peer, aggregated into the engine's own report.
- **A plugin seam** — author an op, a sub-graph or an adapter in Rust and call
  it from Python alongside the built-ins, with a compiled hot core under
  dynamic Python wiring.

---

## Installation

`wingfoil` is not on PyPI yet — the published `wingfoil` package is still
the legacy engine. Until the cutover, install from a source checkout:

```bash
git clone https://github.com/wingfoil-io/wingfoil
cd wingfoil/crates/wingfoil-python
pip install maturin
maturin develop
```

That builds the wheel's full adapter set (PostgreSQL, Kafka, Redis, etcd,
Fluvio, CSV, ZeroMQ, OTLP, augurs, KDB+, FIX, Prometheus, web). Two adapters
are opt-in because they would make the build platform-specific:

```bash
maturin develop -F iceoryx2       # Linux/POSIX shared memory
maturin develop -F aeron          # needs clang, libuuid, CMake >= 3.30
```

`protoc` is needed at build time (etcd compiles its protos):
`sudo apt-get install -y protobuf-compiler`.

Adapter *clients* are compiled into the extension, so no extra Python packages
are needed for them — only the backing service. `pandas` is needed at run time
for [`stream.dataframe()`](#pandas-integration), and nothing else.

Because the adapters are cargo features, the module surface reflects what you
built. `hasattr(wingfoil, "kafka_sub")` is the check.

---

## Quick Start

```python
import wingfoil as wf

g = wf.Graph()
(
    g.counter(period_nanos=1_000_000_000)   # tick every second: 1, 2, 3, …
     .map(lambda n: f"hello, world {n}")
     .print()                               # print each value, pass it through
)
g.run(cycles=3)
```

```
hello, world 1
hello, world 2
hello, world 3
```

`Graph.run()` blocks until the bound is reached, and defaults to deterministic
historical replay from `t=0` — so the run above finishes instantly rather than
taking three seconds. Pass any of:

| Argument | Type | Meaning |
| --- | --- | --- |
| `realtime` | `bool` | `True` uses wall-clock time; `False` (the default) is historical replay. |
| `start_nanos` | `int` | Historical start, in nanoseconds since the epoch. |
| `duration_nanos` | `int` | Stop after this much graph time. |
| `cycles` | `int` | Stop after this many engine cycles. Takes precedence over `duration_nanos`. |

With neither `cycles` nor `duration_nanos`, the graph runs forever.

---

## Core Concepts

- **Graph** — the builder you hold. Every source is created *on* it
  (`g.counter(...)`, `wf.csv_read(g, ...)`), and `g.run(...)` executes
  everything wired onto it. There is no ambient graph and no per-stream `run`.
- **Stream** — a time-stamped channel of values. Combinators
  (`.map`, `.filter_value`, `.distinct`, …) each return a new `Stream`, so
  pipelines chain. Sinks return a terminal `Stream` whose value is `None`;
  keep hold of it or not, it is wired either way.
- **Value** — after the run, `stream.value()` reads back the last value the
  stream carried.
- **Tick** — a cycle in which a stream produced a value. A combinator that
  drops a value simply does not tick, and its downstreams do not run.

### Run modes and the run window

- `realtime=True` — the engine follows the wall clock. Use it with live inputs
  (sockets, brokers, shared memory).
- `realtime=False` — **historical replay**, driven by event timestamps. The
  graph runs as fast as the CPU allows and time advances purely from source
  events, so it is deterministic: the right mode for tests and backtests.

A Python `Graph` does not know its mode until `run()` is called, but several
adapters need it at *wiring* time — a live subscriber has no timeline to
replay, and a sliced historical read must know its window to build its queries.
Those adapters take the fact as an **argument** (`realtime=`, or `start_nanos=`
/ `duration_nanos=`), and it must match the eventual `run(...)`. Passing
`realtime=False` to a live-only source raises there and then, rather than
producing an empty run.

### Bursts: why a source tick is a list

A **burst** is the group of values sharing one instant — the messages that
arrived between two graph cycles, or the rows of a replay carrying the same
timestamp. The engine never collapses a burst to "latest wins" and never drops
a member, so on the Python edge a burst erases to a **`list`**: one tick, one
list, in arrival order.

That is why every I/O source yields a list per tick, even when the list usually
has one element. Index it for the single-value case:

```python
rows = wf.csv_read(g, "prices.csv", "time")
first = rows.map(lambda batch: batch[0])
```

Sinks accept the same shape in reverse: a `list`/`tuple` writes a multi-value
burst, anything else writes a single-element one.

---

## Stream Operators

Every combinator below is a method on `Stream` unless marked otherwise.

### Sources

Sources are built on the `Graph`.

| Source | Description |
| --- | --- |
| `g.counter(period_nanos)` | Emit the running tick count `1, 2, 3, …` every `period_nanos`. |
| `g.constant(value)` | Emit `value` once, on the first cycle. |
| `g.values(values, period_nanos)` | Replay a finite `list`, one value per tick, `period_nanos` apart. The straightforward way to feed real data in from Python. A graph containing it is single-run. |
| `g.custom_node(upstreams, obj)` | Wire a Python object as a node — see [Python-defined nodes](#python-defined-nodes). |

I/O sources are module-level functions taking the graph first — see
[I/O adapters](#io-adapters).

### Transforming values

| Operator | Description |
| --- | --- |
| `.map(f)` | Apply `f(value)` to each tick. A raised exception aborts the run. |
| `.filter_map(f)` | Map and filter in one: `f(value)` returning `None` drops the tick. |
| `.fold(init, f)` | Fold into an accumulator seeded from `init`, emitting it after each fold. |
| `.reduce(f)` | Like `fold`, but the first value seeds the accumulator. |
| `.difference()` | Emit `value - previous` (quiet on the first). |
| `.pairwise()` | Emit `(previous, current)` tuples (quiet on the first); works for non-arithmetic values. |
| `.neg()` | Arithmetic negation — Python `-value` / `__neg__` (`5 -> -5`). **Not** a logical `not` (`True -> -1`, not `False`) and **not** a bitwise `~` (`5 -> -5`, not `-6`); for those use `.map(lambda v: not v)` or `.map(lambda v: ~v)`. |
| `.bimap(other, f)` | Combine two streams through `f(this, other)`, whenever either ticks. |

### Gating and rate control

| Operator | Description |
| --- | --- |
| `.filter_value(pred)` | Keep a value only when `pred(value)` is truthy. **This is legacy `wingfoil`'s `filter`.** |
| `.filter(condition)` | Emit only while another *stream*'s current value is truthy. Takes a `Stream`, not a callable. |
| `.filter_none()` | Drop values that are `None`. |
| `.distinct()` | Suppress consecutive duplicates — emit on change only. |
| `.drop_small_change(is_small)` | Suppress ticks while `is_small(current, last_emitted)` is truthy. Compares against the last value *emitted*, so a slow drift still eventually ticks. |
| `.limit(n)` | Pass the first `n` values through, then stay quiet. |
| `.skip(n)` | Suppress the first `n` values, then pass every later value through. |
| `.step_by(n)` | Emit the first value, then every `n`th value; `n` must be greater than zero. |
| `.throttle(interval_nanos)` | Emit at most once per interval. |
| `.sample(trigger)` | Re-emit the current value whenever `trigger` ticks. |
| `.delay(delay_nanos)` | Re-emit each value that many nanoseconds later. |

### Combining and splitting

| Operator | Description |
| --- | --- |
| `.merge(other)` | Merge two streams; on a same-cycle tie the earliest-supplied input wins. |
| `.merge_all([...])` | Merge several at once. |
| `.split()` | Decompose a stream of 2-tuples into two streams. |

### Aggregation

| Operator | Description |
| --- | --- |
| `.count()` | Emit the running tick count, ignoring values. |
| `.sum()` / `.mean()` | Cumulative running sum / mean. `.average()` is an alias of `.mean()`. |
| `.accumulate()` | Collect every value into a growing `list`, re-emitted each tick. |
| `.buffer(capacity)` | Flush a `list` once `capacity` values accumulate (and on the last cycle). |
| `.window(interval_nanos)` | Flush a `list` on each time boundary (and on the last cycle). |
| `.collect()` | Collect every `(nanos, value)` pair into a growing list of tuples. |
| `.with_time()` | Pair each value with engine time as `(nanos, value)`. |
| `.dataframe()` | Build a pandas `DataFrame` — see [Pandas integration](#pandas-integration). |

### Observing and reading back

| Operator | Description |
| --- | --- |
| `.inspect(f)` | Call `f(value)` and pass the value through. The pass-through tap (legacy's `for_each`). |
| `.print()` | Print each value to stdout, passing it through. |
| `.logged(label, level="info")` | Log `"{time} {label} {value}"` and pass through. `level` is `"trace"`/`"debug"`/`"info"`/`"warn"`/`"error"`. |
| `.value()` | After the run, the stream's last value (legacy's `peek_value`). |

### Statistics

Cumulative `.sum()` and `.mean()` are `Stream` methods. The full windowed
moment surface — rolling `variance`, `std`, `median`, `min`/`max`, time- and
count-weighted windows, and `ewma` — lives in the engine's Rust `adapters::statistics` layer
and reaches Python through the [plugin seam](#authoring-components-in-rust),
as a `#[pyop]` exposing exactly the window you want. That keeps the
parameterisation where it can be type-checked rather than in extra Python
classes. This is the one place the binding is currently narrower than legacy's;
the engine already has the whole surface.

### Example: most operators in one pipeline

```python
import wingfoil as wf

g = wf.Graph()
avg_of_odds = (
    g.counter(period_nanos=100_000_000)   # 1, 2, 3, … every 100ms of graph time
     .filter_value(lambda n: n % 2 == 1)  # 1, 3, 5, …
     .map(float)
     .mean()                              # cumulative running mean
     .logged("avg")
)
g.run(cycles=10)
print("last:", avg_of_odds.value())       # 5.0
```

---

## Python-defined nodes

A Python object can *be* a graph node. There are two forms over the same
machinery.

**Composition** — pass any object implementing `cycle(values) -> bool` (given
the upstreams' current values, return whether it ticked) and `peek()` (its
output when it did):

```python
class RunningMax:
    def __init__(self):
        self.best = None

    def cycle(self, values):
        for value in values:
            if value is not None and (self.best is None or value > self.best):
                self.best = value
        return self.best is not None

    def peek(self):
        return self.best

g = wf.Graph()
source = g.counter(period_nanos=100)
peak = g.custom_node([source], RunningMax()).print()
g.run(cycles=5)
```

**Inheritance** — legacy wingfoil's shape. The constructor call *is* the wiring
step and returns the wired `Stream`, so it chains:

```python
class Polynomial(wf.CustomStream):
    """Sum of upstream[i] * 10**i."""

    def cycle(self):
        value = sum(
            (u.peek_value() or 0) * 10**i for i, u in enumerate(self.upstreams())
        )
        self.set_value(value)
        return True

g = wf.Graph()
source = g.counter(period_nanos=100)
Polynomial(g, [source] * 3).map(lambda x: x * 0.01).logged("poly")
g.run(cycles=5)
```

Two deviations from legacy, both forced by the engine rather than chosen:

- **The graph is explicit** — `MyStream(graph, upstreams)`. Wingfoil has no ambient
  graph, and a `Stream` carries no reference back to its builder.
- **`upstreams()` yields value snapshots**, not the upstream `Stream` objects.
  During a run the engine holds its runner mutably borrowed, so a Python
  `cycle` cannot call back into the graph to read a sibling. The current values
  are handed in, wrapped in objects exposing `peek_value()` — the only stream
  method legacy `cycle` bodies use. A not-yet-ticked upstream reads as `None`.

A graph containing a Python-defined node is **single-run**: the instance's
state is caller-owned and the engine has no hook to reset it, so a second
`run()` raises rather than replaying from dirty state.

---

## Backtesting with historical mode

Historical replay is the default, and it is deterministic — the right mode for
unit tests and strategy backtests. Time advances from source events rather than
the wall clock, so a graph over a day of data finishes as fast as the CPU
allows.

```python
import wingfoil as wf

MINUTE_NANOS = 60_000_000_000
JAN_2025 = 1_735_689_600_000_000_000   # 2025-01-01T00:00:00Z

g = wf.Graph()
average = g.counter(period_nanos=1_000_000_000).map(float).mean()
g.run(start_nanos=JAN_2025, duration_nanos=MINUTE_NANOS)
print(average.value())                 # 31.5 — a minute of ticks, replayed instantly
```

Keep an eye on what the graph *retains*: `.accumulate()` and `.collect()` grow a
list per tick, so over a long replay prefer a running aggregate (as above) or a
windowed flush (`.buffer(n)` / `.window(interval_nanos)`).

Sources that read a bounded slice of history (`postgres_read`, `kdb_read`) take
the same window at wiring, and it must match the `run(...)` call:

```python
rows = wf.postgres_read(
    g, CONN_STR, "SELECT time, sym, price FROM trades", "time",
    start_nanos=JAN_2025, duration_nanos=DAY_NANOS,
)
g.run(start_nanos=JAN_2025, duration_nanos=DAY_NANOS)
```

---

## Pandas integration

`stream.dataframe()` accumulates each value with its engine time and, on the
final cycle, produces a `pandas.DataFrame` (columns `time`, `value`) as the
stream's value. The frame is built in Rust, so there is no Python-side
assembly step:

```python
import wingfoil as wf

g = wf.Graph()
frame = g.counter(period_nanos=10_000_000).map(lambda n: n * n).dataframe()
g.run(cycles=5)
print(frame.value())
#        time  value
# 0         0      1
# 1  10000000      4
# 2  20000000      9
# ...
```

For a frame of several streams, map them into one dict-valued stream before
framing it, or collect each with `.collect()` and join in pandas. (Legacy's
multi-stream `build_dataframe` has no direct equivalent yet.)

---

## Latency tracing

Stamp wall-clock timestamps onto messages as they hop through a graph — and
across processes — then aggregate the per-stage deltas at the end of the
pipeline. This surface is always present; it is not an adapter, so there is no
feature to enable.

```python
import wingfoil as wf

stages = ["ingest", "decode", "publish"]

g = wf.Graph()
messages = source.map(lambda payload: wf.TracedBytes(payload, wf.Latency(stages)))

# All three stages from one node, one GIL attach:
stamped = wf.stamp_all(messages, stages, "precise")
sink, stats = wf.latency_report(stamped, stages, output="silent")

g.run(cycles=1000)
print(stats["decode"]["p99_ns"])
print(stats.total["p99_ns"])   # end to end
print(stats.report())
```

- `stamp` reads the cycle-start clock; `stamp_precise` takes a fresh clock read
  per tick, for intra-cycle resolution. `stamp_as(stream, stage, mode)` takes
  the choice as an argument — `"off"`, `"cycle"` or `"precise"` — which is what
  a config flag wants; the named forms are shorthands for it.
- `stamp_all(stream, stages, mode)` writes several stages from **one** node, in
  list order. Identical to chaining `stamp_as` per stage — a fresh clock read
  each under `"precise"`, one shared snap under `"cycle"` — but it visits the
  values once instead of once per stage, so N stamps cost one GIL attach.
- Toggling: for `stamp_as`/`stamp_all`, pass `mode="off"` and nothing is
  wired — the stream comes back unchanged, so call sites do not branch. The
  named forms (`stamp`, `stamp_precise`, `latency_report`) instead have an
  `_if(..., enabled)` variant that does the same thing.
- `latency_report` returns a **tuple** `(sink, LatencyStats)`; the stats handle
  is live, so the numbers are readable after the run. `output` picks where the
  teardown summary goes: `"stdout"`, `"log"` or `"silent"`.
- `stats` reads out as `stats["<stage>"]` (the hop ending there), `stats.hops()`
  (all of them, labelled) and `stats.total` (first stage to last — a number no
  sum of the hops can produce). `stats.reset()` drops the samples, which is how
  a cumulative p99 becomes a windowed one: without it, one outlier pins the
  figure for the rest of the run.
- A hop that produced no measurement is **tallied, not dropped**: each entry
  carries `same_instant` (both stages in one engine cycle — stamp precisely),
  `backwards` (the clocks disagree) and `unstamped` (not instrumented). A
  `count` below the message count is therefore explainable.
- Bursts are stamped **element-wise**: a value reaching `stamp` may be a list of
  `TracedBytes`, and every member is stamped under one GIL attach.
- `Latency.to_bytes()` / `Latency.from_bytes(data, stages)` are the
  little-endian header a Rust peer reads straight back as its `latency_stages!`
  record — and `iceoryx2_sub` / `iceoryx2_pub` will carry it for you, given a
  `stages=` list.
- Aggregation and report formatting are the engine's own, so a Python report is
  byte-identical to a Rust one.

---

## I/O adapters

Every adapter is a module-level function (or, where it is stateful, a class)
taking the graph or the stream as its first argument — never a `Stream` method.
That is deliberate: a binding authored in a third-party crate cannot add a
method to a `#[pyclass]` defined here, so making the built-ins free functions is
what makes a third-party adapter indistinguishable from a built-in one.

Two conventions run through all of them:

**Selectors are strings, not enum classes.** A poll mode, a service variant, an
etcd event kind, an augurs model — each is a plain `str`, and a wrong value
raises listing the accepted set.

**Conversion fails loudly.** A missing required field, a wrong-typed value, or a
non-`dict` where a record was expected aborts the run naming the offending
field, rather than defaulting to empty bytes or an empty burst.

Every entry point carries a full docstring — `help(wf.kafka_sub)` — and the
[API reference](https://github.com/wingfoil-io/wingfoil/blob/next/crates/wingfoil-python/docs/api.rst) tabulates the whole surface.

### PostgreSQL

`postgres_read` (sliced historical replay), `postgres_sub` (a live
`LISTEN`/`NOTIFY` tail), `postgres_source` (one wiring call for either mode),
`postgres_write`, and `postgres_notify_trigger_sql()` for the trigger DDL. Rows
are `dict`s; a tick is a `list` of rows.

```python
import wingfoil as wf

CONN_STR = "host=localhost user=postgres password=postgres dbname=postgres"
DAY_NANOS = 86_400_000_000_000

g = wf.Graph()

# Historical, time-sliced replay.
rows = wf.postgres_read(
    g, CONN_STR, "SELECT time, sym, price FROM trades", "time",
    start_nanos=START, duration_nanos=DAY_NANOS, chunk_secs=86400,
)
rows.map(lambda batch: [r["price"] for r in batch]).print()
g.run(start_nanos=START, duration_nanos=DAY_NANOS)
```

```python
# Streaming insert sink — declare the target columns in table order.
wf.postgres_write(
    trades, CONN_STR, "trades", [("sym", "text"), ("price", "float"), ("qty", "long")]
)
```

### Kafka

Events are `{topic, partition, offset, key, value}` dicts. `topic` is optional
on the sink — a record naming its own target lets one sink write to many topics.

```python
BROKERS = "localhost:9092"

produce = wf.Graph()
records = produce.values(
    [{"topic": "trades", "key": b"k", "value": b"alpha"}], period_nanos=1_000_000_000
)
wf.kafka_pub(records, BROKERS)
produce.run(cycles=1)

consume = wf.Graph()
wf.kafka_sub(consume, BROKERS, "trades", "my-group").inspect(print)
consume.run(realtime=True, duration_nanos=10_000_000_000)
```

### Redis

Pub/Sub (`redis_sub` / `redis_pub`, events `{channel, payload}`) and persistent
Streams (`redis_stream_read` / `redis_stream_write`, events `{key, id, fields}`).
`channel` / `key` are optional fallbacks on the sinks; both sinks take a
`buffer_size` back-pressure bound.

```python
URL = "redis://127.0.0.1:6379"

# Subscribe, uppercase, republish — Redis Pub/Sub is fire-and-forget, so the
# subscriber must be live before anything is published.
g = wf.Graph()
inbound = wf.redis_sub(g, URL, "source")
outbound = inbound.map(
    lambda batch: [{"channel": "dest", "payload": e["payload"].upper()} for e in batch]
)
wf.redis_pub(outbound, URL)
g.run(realtime=True, duration_nanos=5_000_000_000)
```

### etcd

Events are `{kind, key, value, revision}` with `kind` a string (`"put"` /
`"delete"`). `endpoints` accepts a single `str` or a list, so a cluster is
addressable.

```python
ENDPOINT = "http://localhost:2379"

g = wf.Graph()

# Publish: each dict is {"key": str, "value": bytes}, or a list of them per tick.
entries = g.values(
    [{"key": "/wf/item/1", "value": b"1"}], period_nanos=1_000_000_000
)
wf.etcd_pub(entries, ENDPOINT, lease_ttl_secs=30.0, force=True)

# Subscribe: snapshot + watch under a prefix; each tick is a list of events.
wf.etcd_sub(g, ENDPOINT, "/wf/").inspect(print)

g.run(realtime=True, duration_nanos=2_000_000_000)
```

### Fluvio

Events are `{key, value, offset}`. The key is **asymmetric** — a read yields
`bytes | None`, a write expects `str | None` — because that is the adapter's own
shape, so a round trip needs an explicit `.decode()`.

```python
g = wf.Graph()
wf.fluvio_sub(g, "127.0.0.1:9003", "source", partition=0).inspect(print)
g.run(realtime=True, duration_nanos=5_000_000_000)
```

### CSV

Deterministic historical replay. Values are `str` on both sides (CSV has no
types), column order follows the file header, and `buffer_size` bounds the
replay look-ahead so a huge file is not read up front.

```python
g = wf.Graph()

# Read: `time` holds integer nanoseconds since the epoch. Each tick is a list
# of the rows sharing that timestamp.
rows = wf.csv_read(g, "prices.csv", "time")
rows.inspect(print)

g.run(start_nanos=0, duration_nanos=5_000_000_000)
```

```python
# Write: the header is explicit — a dynamic row has no field names to derive
# one from. The graph timestamp is written as a leading `time` column.
g = wf.Graph()
quotes = g.values(
    [{"sym": "AAPL", "price": "101.0"}, {"sym": "AAPL", "price": "102.0"}],
    period_nanos=100_000_000,
)
wf.csv_write(quotes, "out.csv", ["sym", "price"])
g.run(cycles=2)
```

### KDB+

`kdb_read` (a time-sliced historical query), `kdb_sub` (the tickerplant tail —
new against legacy) and `kdb_write`. Rows are `dict`s dispatched on each value's
actual KDB type; temporal columns decode to their **raw `int`** (nanoseconds
from the KDB epoch, 2000-01-01) rather than a `datetime` that would have to
guess a timezone.

Start a q process (`q -p 5000`) and create the table:

```
test_trades:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())
```

```python
HOST, PORT, TABLE = "localhost", 5000, "test_trades"
KDB_EPOCH_NANOS = 946_684_800_000_000_000
DAY_NANOS = 86_400_000_000_000

# Write: each dict is one row; `columns` names them in table order.
g = wf.Graph()
trades = g.values(
    [{"sym": "AAPL", "price": 100.0, "qty": 10}], period_nanos=1_000_000_000
)
wf.kdb_write(
    trades, HOST, PORT, TABLE,
    [("sym", "symbol"), ("price", "float"), ("qty", "long")],
)
g.run(cycles=1, start_nanos=KDB_EPOCH_NANOS)

# Read: a time-sliced query over the same window the run declares.
g = wf.Graph()
rows = wf.kdb_read(
    g, HOST, PORT, f"select from {TABLE}", "time",
    KDB_EPOCH_NANOS, DAY_NANOS, chunk_secs=3600,
)
rows.inspect(print)
g.run(start_nanos=KDB_EPOCH_NANOS, duration_nanos=DAY_NANOS)
```

### ZeroMQ

`zmq_sub` returns a **tuple** `(data, status)`; `status` ticks only on a
transition, carrying `"connected"` / `"disconnected"`. Payloads cross as
`bytes`. The `_etcd` pair adds service discovery and needs the `etcd` feature
too.

```python
g = wf.Graph()

counter = g.counter(period_nanos=20_000_000)
wf.zmq_pub(counter.map(lambda n: str(n).encode()), 7779, bind_address="127.0.0.1")

data, status = wf.zmq_sub(g, "tcp://127.0.0.1:7779")
data.inspect(lambda messages: [print("msg:", m) for m in messages])
status.inspect(lambda s: print("status:", s))

g.run(realtime=True, duration_nanos=2_000_000_000)
```

A `SUB` socket is a slow joiner — messages published before it finishes
connecting are simply lost — so assert on what arrived, never on the first
message.

With etcd discovery, publishers register under a service name and subscribers
look it up; the lookup happens at wiring, so an unreachable registry raises
there:

```python
wf.zmq_pub_etcd(payloads, 7779, "quotes", "http://127.0.0.1:2379",
                bind_address="127.0.0.1")
data, status = wf.zmq_sub_etcd(g, "quotes", "http://127.0.0.1:2379")
```

### FIX 4.4

`fix_connect` (initiator), `fix_accept` (acceptor), `fix_connect_tls` (TLS
initiator, handing back a `FixConnection`) and `fix_send`. Each source returns
`(data, status)`. Messages are
`{"msg_type": str, "seq_num": int, "fields": [(tag, value)]}` with **`str` tag
values** — FIX is a text protocol, so spell a number `str(price)`. Session
status is always a dict.

```python
g = wf.Graph()

data, status = wf.fix_connect(g, "fix.example.com", 9876, "MYCOMP", "BROKER")
data.inspect(lambda messages: [print("fix:", m) for m in messages])
status.inspect(print)

g.run(realtime=True, duration_nanos=10_000_000_000)
```

```python
# TLS initiator (e.g. LMAX). This one hands back a single `FixConnection`
# exposing `.data`, `.status`, `.send(msg)` and `.fix_sub(symbols)`.
g = wf.Graph()
connection = wf.fix_connect_tls(
    g, "fix-marketdata.london-digital.lmax.com", 443, "USERNAME", "LMXBL",
    password="secret",
)
connection.data.inspect(print)
connection.send({"msg_type": "V", "fields": [(262, "req1"), (263, "1"), (264, "0")]})
```

### augurs

Six on-graph time-series operators, no external service. `augurs_forecast`,
`augurs_changepoint` and `augurs_seasons` analyse **one** series (a stream of
floats); `augurs_outlier`, `augurs_dtw` and `augurs_cluster` compare **several**
(a stream of lists of floats). Results are `dict`s carrying the full model
output, not just the headline number, and `model` / `detector` / `metric` are
strings.

```python
g = wf.Graph()
prices = g.values([float(i) for i in range(1, 41)], period_nanos=1_000_000_000)

wf.augurs_forecast(prices, window=32, horizon=3, level=0.95).inspect(
    lambda f: print(f["point"], f["lower"], f["upper"])
)
wf.augurs_changepoint(prices, window=32).inspect(print)

g.run(cycles=40)
```

### Prometheus

A stateful handle exposing a gauge per stream on a scrape endpoint. Historical
runs are a **no-op** — a backtest never publishes fast-forwarded values to a
live endpoint.

```python
g = wf.Graph()

exporter = wf.PrometheusExporter("0.0.0.0:9091")
port = exporter.serve()                       # bind and start the HTTP server
exporter.gauge("wingfoil_ticks", g.counter(period_nanos=1_000_000_000))

g.run(realtime=True, duration_nanos=5_000_000_000)
```

Scrape with `curl http://localhost:9091/metrics`.

### OpenTelemetry OTLP

Push a stream's value to an OTLP HTTP collector as a gauge, stringified via
Python's `str()`. Historical runs are a no-op, as above.

```python
g = wf.Graph()
wf.otlp_push(
    g.counter(period_nanos=1_000_000_000), "wingfoil_ticks",
    "http://localhost:4318", "demo",
)
g.run(realtime=True, duration_nanos=10_000_000_000)
```

### web (WebSocket)

A stateful `WebServer` handle. Publishing is a **server** method, not a stream
method, because the handle owns the topic registry. Values marshal through
`serde_json::Value`, serialized with the selected codec (`"bincode"` or
`"json"`).

**Use `codec="json"` unless you know otherwise.** `sub` rejects `"bincode"`
outright: it decodes into `serde_json::Value`, whose `Deserialize` calls
`deserialize_any`, which bincode refuses for every value of every shape — so a
Python subscription could never read a frame from any peer, Rust or otherwise.
`pub` still accepts bincode, because it is peer-dependent rather than
impossible: a scalar or a same-width sequence reaches a typed Rust peer
correctly, while a `dict` sent to a Rust `struct` decodes as silent garbage.

`bytes` become an array of ints, which is wire-compatible with a Rust
`Vec<u8>` peer **under JSON only** — `Value` writes each element as a `u64`,
so the bincode encoding does not match. A subscription decodes such a frame
back as a `list`, not `bytes`, since nothing on the wire distinguishes them.

```python
g = wf.Graph()

server = wf.WebServer("127.0.0.1:0", codec="json")
print("listening on", server.port(), "codec", server.codec_name())

server.pub(g.counter(period_nanos=50_000_000), "ticks")
server.pub_bursts(g.constant([1.0, 2.0]), "book")   # a whole burst as one frame
events = server.sub(g, "ui").accumulate()

g.run(realtime=True, duration_nanos=5_000_000_000)
server.stop()
```

`static_dir=` serves a directory alongside the socket, and `cert_path=` /
`key_path=` (given together) switch it to TLS.

### Aeron

`aeron_sub`, `aeron_sub_with_status`, `aeron_pub`, `aeron_pub_with_status`.
**Not in the default build** — `rusteron-client` builds the Aeron C library from
source. Connecting is *eager*: an unreachable media driver raises at wiring.
`mode` is `"spin"` or `"threaded"`.

```python
g = wf.Graph()
messages = wf.aeron_sub(g, "aeron:ipc", 1001, mode="spin")
wf.aeron_pub(messages, "aeron:ipc", 1002)
g.run(realtime=True, duration_nanos=5_000_000_000)
```

### iceoryx2

Zero-copy pub/sub over shared memory. **Not in the default build** —
Linux/POSIX-only. Payloads cross as `bytes`; `variant` (`"ipc"` / `"local"`) and
`mode` (`"spin"` / `"threaded"` / `"signaled"`) are strings.

```python
g = wf.Graph()

received = wf.iceoryx2_sub(g, "wingfoil/demo", variant="local", mode="signaled")
received.inspect(lambda messages: print("received:", messages))

wf.iceoryx2_pub(
    g.counter(period_nanos=100_000_000).map(lambda n: f"tick {n}".encode()),
    "wingfoil/demo", variant="local",
)

g.run(realtime=True, duration_nanos=500_000_000)
```

Both entry points take an optional `stages=` list. With it, a sample is a
`[u64; len(stages)]` little-endian stamp header followed by the payload, and the
value is a `TracedBytes` rather than `bytes` — the same layout a Rust peer's
`latency_stages!` record has, so a Python subscriber reads a Rust publisher's
stamps.

---

## Authoring components in Rust

The point of the binding is not just to expose the built-ins: you can author an
op, a whole sub-graph, or an I/O adapter **in Rust** and call it from Python
alongside the built-in vocabulary. Three macros do it, and a binding written in
a third-party crate is indistinguishable from a built-in one:

| Macro | Exposes |
| --- | --- |
| `#[pyop]` / `pyop_fn!` | An `Op` implementation as `module.my_op(stream, …)`. One to four inputs, tuple `Cfg` (each element its own named Python parameter), and stateful ops. |
| `#[pygraph]` | A Rust wiring function (`fn(&Stream<T>) -> Stream<U>`) as one callable that splices its nodes into the caller's graph. Multi-input and tuple-returning forms supported. |
| `#[pyadapter]` | A source or sink adapter trait on `GraphBuilder` or `Stream<T>`. A tuple return gives the `(data, status)` shape live sources use. |

The interior of any of these stays **natively typed** — only the Python-facing
edge erases to the boxed `PyElement`. Combined with `compiled_island`, that
gives "compiled interiors, dynamic wiring": Python composes the graph at run
time while the hot sub-graphs run as monomorphized straight-line code.

This crate ships demo components proving the seam end to end — `scale`,
`square`, `running_total`, `weighted_add`, `blend3`, `blend4`, `clamped_scale`,
`doubled_running_total`, `spread_and_mid`, `ramp_source`, `pair_source`,
`split_source`, `list_sink`, `burst_list_sink`, `compiled_island` and
`interpreted_twin`:

```python
import wingfoil as wf

g = wf.Graph()
ramp = wf.ramp_source(g, 10.0, 2.0)     # a Rust source adapter
squared = wf.square(ramp)               # a Rust op
totals = wf.doubled_running_total(ramp) # a Rust sub-graph

collected = []
wf.list_sink(squared, collected)        # a Rust sink adapter
g.run(cycles=3)
print(collected)                        # [100.0, 144.0, 196.0]
```

See `docs/python-interop.md` at the repository root for the design, and
`examples/plugin_sdk.py` for the runnable version.

---

## Build from source

```bash
cd crates/wingfoil-python
maturin develop                   # build + install into the active environment
maturin build --out dist          # or build a wheel
pip install --force-reinstall dist/*.whl
```

`maturin develop -F <feature>` **replaces** the wheel's feature list rather
than adding to it, so a lone `-F aeron` build carries only that adapter.

---

## Testing

```bash
pytest                            # Python round-trip tests

# the Rust object form and boundary type have their own unit tests
# (`--features all-adapters` also covers the per-adapter marshaling tests):
cargo test -p wingfoil-python --features all-adapters
```

Adapter integration tests are marked (`@pytest.mark.requires_postgres`) and
deselected by default, so a plain `pytest` is never silently green against a
service that is not up. Opt in explicitly with a service running:

```bash
docker run --rm -p 5432:5432 -e POSTGRES_PASSWORD=postgres postgres:16-alpine
pytest -m requires_postgres tests/test_postgres.py
```

Runnable examples live in [`examples/`](https://github.com/wingfoil-io/wingfoil/tree/next/crates/wingfoil-python/examples) and are smoke-tested by
`tests/test_examples.py`, so they stay working as the binding evolves.

---

## Documentation

- [`docs/`](https://github.com/wingfoil-io/wingfoil/tree/next/crates/wingfoil-python/docs) — the Sphinx source for the published module documentation:
  this guide, the [API reference](https://github.com/wingfoil-io/wingfoil/blob/next/crates/wingfoil-python/docs/api.rst), and the
  [migration guide](https://github.com/wingfoil-io/wingfoil/blob/next/crates/wingfoil-python/docs/migration.rst) for the legacy `wingfoil` package.
  Build it with `maturin develop` followed by `make html` in `docs/`; see
  [`docs/README.md`](https://github.com/wingfoil-io/wingfoil/blob/next/crates/wingfoil-python/docs/README.md).
- Every adapter's module docs (`src/adapters/<name>.rs`) carry its entry-point
  table, its argument semantics, and how its surface differs from the legacy
  binding. Those doc comments *are* the Python docstrings — `help(wf.csv_read)`.
- `docs/python-interop.md` at the repository root — the design of the boundary
  and the plugin seam.

---

## Release status and feedback

Wingfoil is pre-release: it is the engine that replaces the shipping
`wingfoil`, and the Python binding tracks it. APIs are stabilising and we would
love your input — especially if you:

- are interested in contributing,
- know of a project Wingfoil is a good fit for,
- want to request a feature, or
- have any feedback.

Email us at [hello@wingfoil.io](mailto:hello@wingfoil.io), ping us on
[discord](https://discord.gg/WfZwpQnZUA), open a
[GitHub discussion](https://github.com/wingfoil-io/wingfoil/discussions/), or
browse the [issue tracker](https://github.com/wingfoil-io/wingfoil/issues).

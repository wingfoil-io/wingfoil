# wingfoil Python examples

Runnable demonstrations of the `wingfoil` binding. Build the extension
module first, then run any example:

```sh
cd crates/wingfoil-python
maturin develop            # builds + installs the `wingfoil` module
python examples/quick_start.py
```

## Core

No services, no optional features — `maturin develop` is enough.

| Example | Shows |
|---|---|
| `quick_start.py`             | build a graph, chain combinators, run, read a value |
| `custom_stream.py`           | a Python object as a graph node (`Graph.custom_node`) |
| `custom_stream_subclass.py`  | the same thing by subclassing `CustomStream` — legacy wingfoil's shape |
| `combine.py`                 | `bimap` two streams running at different rates; `sample` a constant |
| `deduplicate.py`             | `distinct` drops consecutive repeats |
| `delay_line.py`              | `delay` re-emits on the graph clock, `with_time` makes the offset visible |
| `latency.py`                 | stamp a `TracedBytes` through a pipeline, read the per-hop report |
| `dataframe.py`               | collect a stream into a pandas DataFrame, and outer-join several with `build_dataframe` (needs `pandas`) |
| `plugin_sdk.py`              | compose Rust-authored ops / sub-graphs / adapters from Python |

All nine are smoke-tested in `tests/test_examples.py`, so they stay working as
the binding evolves.

## Adapters

Each needs its cargo feature at build time (`maturin develop --features …`)
and a service to talk to; see the docstring at the top of each file.

| Example | Feature | Needs |
|---|---|---|
| `kdb.py`               | `kdb`      | a KDB+ instance on port 5000 |
| `iceoryx2_pubsub.py`   | `iceoryx2` | nothing — publisher and subscriber share one graph |
| `zmq/direct/zmq_pub.py`, `zmq/direct/zmq_sub.py` | `zmq` | each other, in two terminals |
| `zmq/etcd/zmq_pub.py`, `zmq/etcd/zmq_sub.py`     | `zmq,etcd` | each other, plus a local etcd |

The zmq pairs are wire-compatible with the Rust `zmq` example — a Python
publisher feeds a Rust subscriber and the reverse, which is what
`tests/zmq_cross_lang_integration.rs` pins. Point one at the other's port: the
Rust example uses 5556, these use 7779.

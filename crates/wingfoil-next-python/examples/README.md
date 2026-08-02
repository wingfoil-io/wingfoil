# wingfoil-next Python examples

Runnable demonstrations of the `wingfoil_next` binding. Build the extension
module first, then run any example:

```sh
cd crates/wingfoil-next-python
maturin develop            # builds + installs the `wingfoil_next` module
python examples/quick_start.py
```

| Example | Shows |
|---|---|
| `quick_start.py`             | build a graph, chain combinators, run, read a value |
| `custom_stream.py`           | a Python object as a graph node (`Graph.custom_node`) |
| `custom_stream_subclass.py`  | the same thing by subclassing `CustomStream` — legacy wingfoil's shape |
| `dataframe.py`               | collect a stream into a pandas DataFrame, and outer-join several with `build_dataframe` (needs `pandas`) |
| `plugin_sdk.py`              | compose Rust-authored ops / sub-graphs / adapters from Python |

All five are smoke-tested in `tests/test_examples.py`, so they stay working as
the binding evolves.

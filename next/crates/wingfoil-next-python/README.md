# wingfoil-next-python

Python interop prototype for **wingfoil-next** (the Op-pattern engine under
`next/`). See `next/docs/python-interop.md` for the design.

The importable module is `wingfoil_next`, exposing `Graph` and `Stream`:

```python
import wingfoil_next as wf

g = wf.Graph()
out = g.counter(period_nanos=100).map(lambda n: n * 2)
g.run(cycles=3)          # historical replay from t=0
assert out.value() == 6  # 3rd tick -> 3 * 2
```

`map` takes a native Python callable; a raised exception aborts the run. Values
cross the boundary as native Python objects and are boxed into the erased
`PyElement` only at the edges.

## I/O adapters

The `wingfoil_next::adapters::*` adapters are exposed as module-level functions
(`src/adapters/`), each behind a cargo feature of the same name — a wheel only
carries the adapters it was built with. **postgres** is bound today:

```python
g = wf.Graph()

# Historical, time-sliced replay. Each tick is a list of {column: value} dicts.
rows = wf.postgres_read(
    g, CONN_STR, "SELECT time, sym, price FROM trades", "time",
    start_nanos=START, duration_nanos=ONE_DAY, chunk_secs=86400,
)
g.run(start_nanos=START, duration_nanos=ONE_DAY)

# Streaming insert sink — declare the target columns in table order.
wf.postgres_write(stream, CONN_STR, "trades",
                  [("sym", "text"), ("price", "float")])
```

Also `postgres_sub` (a real-time `LISTEN`/`NOTIFY` live tail), `postgres_source`
(one wiring call for either run mode) and `postgres_notify_trigger_sql`. See the
module docs in `src/adapters/postgres.rs` for the argument semantics and how the
surface differs from the legacy `wingfoil-python` bindings.

A source that needs the run window at wiring takes it as arguments
(`start_nanos` / `duration_nanos` / `realtime`), which must match the eventual
`graph.run(...)` — a Python `Graph` does not know its run mode until then.

## Build / test

```bash
# from this directory
maturin build --out dist          # build the wheel
pip install --force-reinstall dist/*.whl
pytest                            # Python round-trip tests

# the Rust object form and boundary type have their own unit tests
# (`--features all-adapters` also covers the per-adapter marshaling tests):
cargo test -p wingfoil-next-python --features all-adapters
```

Adapter integration tests are marked (`@pytest.mark.requires_postgres`) and
deselected by default, so a plain `pytest` is never silently green against a
service that is not up. Opt in explicitly with a service running:

```bash
docker run --rm -p 5432:5432 -e POSTGRES_PASSWORD=postgres postgres:16-alpine
pytest -m requires_postgres tests/test_postgres.py
```

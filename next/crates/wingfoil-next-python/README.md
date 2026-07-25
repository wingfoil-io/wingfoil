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

## Build / test

```bash
# from this directory
maturin build --out dist          # build the wheel
pip install --force-reinstall dist/*.whl
pytest                            # Python round-trip tests

# the Rust object form and boundary type have their own unit tests:
cargo test -p wingfoil-next-python
```

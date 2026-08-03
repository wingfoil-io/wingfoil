# wingfoil-python-derive

**`#[pyop]`** — derive a Python-callable function from an `Op` impl.

This is the Python-side counterpart to [`#[op]`](../wingfoil-derive/): where
`#[op]` gives an op its interpreted builder method and compiled forwarders,
`#[pyop]` gives it a Python binding. Together they mean adding an op does not also
mean hand-writing glue for each surface it has to appear on.

## Usage

Placed on an `impl Op for MyOp` — alongside `#[op]`, or on its own — `#[pyop]`
re-emits the impl unchanged and generates a free `#[pyfunction]` that wires the op
onto a `Stream` at the erased boundary.

```rust,ignore
use wingfoil_python::{pyop, Op, Tick, Activation, Ctx};

struct Square;

#[pyop(name = square)]
impl Op for Square {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(_c: &mut (), _s: &mut (), input: (&f64,), _ctx: &mut Ctx)
        -> anyhow::Result<Tick<f64>> { Ok(Tick::Value(input.0 * input.0)) }
}
```

Register the generated function in your `#[pymodule]`:

```rust,ignore
wrap_pyfunction!(square, m)?;
```

and it is callable from Python:

```python
import wingfoil as wf
out = wf.square(stream)
```

It is proc-macro sugar over the `pyop_fn!` declarative macro: instead of spelling
the input, output and config types and the step by hand, they are read off the
`Op` impl's associated types and `cycle`.

## Why a free function, not a method

A user op becomes `module.name(stream[, cfg])` rather than `stream.name()`,
because pyo3 forbids `#[pymethods]` on a foreign pyclass — the same shape the
polars plugin ecosystem uses.

## Scope

One- to four-input **concrete** (non-generic) ops:

| `In<'a>` | Generated signature | Seam |
|---|---|---|
| `(&'a A,)` | `module.name(stream)` | `wire_op1` |
| `(&'a A, &'a B)` | `module.name(stream, other)` | `wire_op2` |
| `(&'a A, &'a B, &'a C)` | `module.name(stream, second, third)` | `wire_op3` |
| `(&'a A, …, &'a D)` | `module.name(stream, second, third, fourth)` | `wire_op4` |

All inputs are **active** — any one ticking runs the op. A passive edge needs a
hand-written method, exactly as on the Rust side. Stream parameters are named, so
they can be passed by keyword.

`Cfg` may be `()` (no config parameter), a single `FromPyObject` type
(`arg = <name>`, defaulting to `cfg`), or a **tuple** destructured into one named
parameter per element with `arg = (<p1>, <p2>, …)` — so an op with
`Cfg = (usize, f64)` reads as `name(stream, window, alpha)`.

## Doc comments carry across

All three macros here — `#[pyop]`, `#[pygraph]` and `#[pyadapter]` — copy the
annotated item's `///` docs onto the generated `#[pyfunction]`, so they become
the callable's **Python docstring**: what `help()` prints, and what the Sphinx
reference in [`../wingfoil-python/docs/`](../wingfoil-python/docs/)
renders for it. There is nowhere else that prose gets written.

Write it once, in Rust, for the *Python* caller: what one tick carries, what
each argument means, what raises at wiring versus what aborts the run.
`#[pyop]` reads the docs off the `impl` block, `#[pygraph]` off the wiring fn,
`#[pyadapter]` off the free fn or the impl's method.

## See also

- [`../README.md`](../README.md) — the crate map for `next/`
- [`../wingfoil-python/`](../wingfoil-python/) — the extension module this feeds
- [`../wingfoil-derive/`](../wingfoil-derive/) — `nitro!` and `#[op]`
- [`../../docs/python-interop.md`](../../docs/python-interop.md) — the interop design
- The `/new-op-next` and `/bind-adapter-next` skills — the step-by-step recipes

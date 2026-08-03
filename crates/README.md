# Wingfoil Next Crates

Four crates. Two carry the engine and its bindings; two are the proc-macro crates
that generate the boilerplate each of those would otherwise need.

```
wingfoil                 the engine — ops, fluent wiring, adapters, runtime core
  └── wingfoil-macros    nitro! and #[op]

wingfoil-python          the Python extension module (`import wingfoil`)
  └── wingfoil-python-macros   #[pyop]
```

| Crate | What it is |
|---|---|
| [**`wingfoil`**](wingfoil/) | The dual-mode stream-processing engine: the `Op` trait and interpreter, the fluent wiring layer, the op catalog, the I/O adapters, and the shared runtime core. |
| [**`wingfoil-macros`**](wingfoil-macros/) | `nitro!` — one wiring function expands to interpreted, compiled and nested runners. `#[op]` — an `Op` impl gains its fluent builder method and the forwarders `nitro!` dispatches through. |
| [**`wingfoil-python`**](wingfoil-python/) | The PyO3 bindings, built with maturin. Importable as `wingfoil`. |
| [**`wingfoil-python-macros`**](wingfoil-python-macros/) | `#[pyop]` — derives a Python-callable function from an `Op` impl, so a new op reaches Python without hand-written glue. |

## Why the macro crates are separate

Proc-macro crates must be their own compilation unit — that is a Rust
requirement, not a design choice. But the split earns its keep: it is what lets a
**user-defined** op take exactly the same path as a built-in one. `#[op]`
generates the interpreted builder method *and* the naming-convention forwarders
that compiled emission dispatches through, so there is no per-op table inside
`nitro!` that would need editing to admit a new op.

The same holds on the Python side: `#[pyop]` means adding an op does not mean
also hand-writing its binding.

## The dependency direction

The edge runs **legacy → next**. `wingfoil` (the legacy crate at the repo root)
depends on `wingfoil` and re-exports the shared runtime core from it.
Nothing under `next/` may depend on `wingfoil` — the cutover *deletes* the legacy
crates, and any such edge would have to be unpicked first.

The one permitted exception is a **dev**-dependency, used for parity tests and
comparison benchmarks against the classic engine.

Shared machinery therefore lives in
[`wingfoil/src/runtime/`](wingfoil/src/runtime/) — engine time, run
bounds, the time queue, `Burst`, the `Kernel`, the latency data layer — and
`wingfoil` re-exports it at its historical path. See
[`../docs/cutover-plan.md`](../docs/cutover-plan.md).

## Where to start

- **Using the engine** → [`wingfoil/examples/`](wingfoil/examples/), and
  [`../README.md`](../README.md) for the overview.
- **Adding an op** → the `/new-op-next` skill, and
  [`../docs/port-plan.md`](../docs/port-plan.md) § "Adding an op".
- **Adding an adapter** → the `/new-adapter-next` skill; then `/bind-adapter-next`
  for its Python bindings.
- **Understanding the design** → [`../docs/`](../docs/) — the port plan, the
  cutover plan, and the design decision records.

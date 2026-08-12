# Wingfoil Crates

Six crates. Two carry the engine and its Python bindings; two are the
proc-macro crates that generate the boilerplate each of those would otherwise
need; two more carry the browser side.

```
wingfoil                 the engine — ops, fluent wiring, adapters, runtime core
  └── wingfoil-derive    nitro! and #[op]

wingfoil-python          the Python extension module (`import wingfoil`)
  └── wingfoil-python-derive   #[pyop]

wingfoil-wire-types      wire format shared by the web adapter and the browser
  └── wingfoil-wasm      the browser-side codec (own workspace, wasm32 target)
```

| Crate | What it is |
|---|---|
| [**`wingfoil`**](wingfoil/) | The dual-mode stream-processing engine: the `Op` trait and interpreter, the fluent wiring layer, the op catalog, the I/O adapters, and the shared runtime core. |
| [**`wingfoil-derive`**](wingfoil-derive/) | `nitro!` — one wiring function expands to interpreted, compiled and nested runners. `#[op]` — an `Op` impl gains its fluent builder method and the forwarders `nitro!` dispatches through. |
| [**`wingfoil-python`**](wingfoil-python/) | The PyO3 bindings, built with maturin. Importable as `wingfoil`. |
| [**`wingfoil-python-derive`**](wingfoil-python-derive/) | `#[pyop]` — derives a Python-callable function from an `Op` impl, so a new op reaches Python without hand-written glue. |
| [**`wingfoil-wire-types`**](wingfoil-wire-types/) | The wire-format types shared by the `web` adapter and the browser client — one definition, so the two ends cannot drift. |
| [**`wingfoil-wasm`**](wingfoil-wasm/) | The browser-side WASM codec behind [`@wingfoil/client`](../js/). Excluded from the default workspace: it targets `wasm32-unknown-unknown`. |

The TypeScript client that consumes the last two is [`js/`](../js/) — an npm
package rather than a Cargo crate, which is why it sits outside this
directory.

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

The edge runs **legacy → wingfoil**. The legacy `wingfoil` crate (under
`legacy/`) depends on this one and re-exports the shared runtime core from it.
Nothing under `crates/` may depend on the legacy crate — the cutover *deletes*
it, and any such edge would have to be unpicked first.

The one permitted exception is a **dev**-dependency, used for parity tests and
comparison benchmarks against the classic engine.

Shared machinery therefore lives in
[`wingfoil/src/runtime/`](wingfoil/src/runtime/) — engine time, run
bounds, the time queue, `Burst`, the `Kernel`, the latency data layer — and
`wingfoil` re-exports it at its historical path. See
[`../docs/planning/cutover-plan.md`](../docs/planning/cutover-plan.md).

## Where to start

- **Using the engine** → [`wingfoil/examples/`](wingfoil/examples/), and
  [`../README.md`](../README.md) for the overview.
- **Adding an op** → the `/new-op` skill, and
  [`../docs/adding-an-op.md`](../docs/adding-an-op.md).
- **Adding an adapter** → the `/new-adapter` skill; then `/bind-adapter`
  for its Python bindings.
- **Understanding the design** → [`../docs/`](../docs/) — the port plan, the
  cutover plan, and the design decision records.

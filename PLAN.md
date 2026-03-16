# Plan: Proc-macro derive for MutableNode and StreamPeekRef (Issue #124)

## Goal

Eliminate boilerplate in custom node definitions by introducing a `wingfoil-derive` proc-macro crate that generates `upstreams()` and `StreamPeekRef` impls from field attributes.

## New User-Facing API

```rust
#[derive(StreamPeekRef)]
struct MyNode {
    upstream: Rc<dyn Stream<Foo>>,
    other_field: BTreeMap<K, V>,
    #[output] value: Baz,   // generates StreamPeekRef<Baz>
}

// MutableNode (upstreams + cycle) is still written manually
impl MutableNode for MyNode {
    fn upstreams(&self) -> UpStreams { ... }
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> { ... }
}
```

---

## Step 1 — Create `wingfoil-derive` crate

- `cargo new --lib wingfoil-derive` at workspace root
- Set `proc-macro = true` in `wingfoil-derive/Cargo.toml`
- Add `syn`, `quote`, `proc-macro2` dependencies
- Add `wingfoil-derive` to `[workspace]` members in root `Cargo.toml`
- Add `wingfoil-derive` as a dependency of `wingfoil`

```toml
# wingfoil-derive/Cargo.toml
[lib]
proc-macro = true

[dependencies]
syn  = { version = "2", features = ["full"] }
quote = "1"
proc-macro2 = "1"
```

---

## Step 2 — Implement `#[derive(StreamPeekRef)]`

The same derive proc-macro (or a separate `#[proc_macro_derive(StreamPeekRef, attributes(output))]`) generates:

```rust
impl StreamPeekRef<Baz> for MyNode {
    fn peek_ref(&self) -> &Baz {
        &self.value
    }
}
```

Rules:
- Exactly one field must carry `#[output]`; zero or >1 is a `compile_error!`
- The output type `T` is inferred from the field's declared type

---

## Step 3 — Re-export from `wingfoil`

In `wingfoil/src/lib.rs`:

```rust
pub use wingfoil_derive::StreamPeekRef;
```

Users need only `use wingfoil::*`.

---

## Step 4 — Migrate trivial `peek_ref` impls (38 nodes)

Replace `impl StreamPeekRef<T> for Foo { fn peek_ref(&self) -> &T { &self.value } }` with `#[output]` on the field and `#[derive(MutableNode)]` (or a standalone `#[derive(StreamPeekRef)]`).

Full list from issue: `average`, `bimap`, `buffer`, `callback`, `channel`, `combine`, `constant`, `delay`, `delay_with_reset`, `demux` (4 impls), `difference`, `distinct`, `feedback`, `filter`, `fold`, `graph_node` (2), `graph_state`, `inspect`, `limit`, `map`, `map_filter`, `merge`, `print`, `producer`, `sample`, `throttle`, `trimap`, `try_bimap`, `try_map`, `try_trimap`, `window`, `iterator_stream` (2), `async_io`.

Nodes with non-trivial `peek_ref` (delegation to an inner stream) are evaluated case-by-case and left as manual impls.

---

## Step 5 — Update docs and examples

- Update `examples/dynamic/dynamism/` to use the derive
- Update custom-node authorship guide in `CONTRIBUTING.md` or `README.md`
- Add a minimal example in `wingfoil-derive/README.md`

---

## Acceptance Criteria (from issue, scoped)

- [ ] `wingfoil-derive` crate compiles with `#[derive(StreamPeekRef)]`
- [ ] `StreamPeekRef<T>` is generated from the `#[output]` field
- [ ] Compile error if zero or more than one `#[output]` field is present
- [ ] All trivial `peek_ref` impls migrated (38 nodes)
- [ ] `examples/dynamic/dynamism/` updated
- [ ] Existing tests pass unchanged

> `upstreams()` and all `MutableNode` impls remain manual — deferred to a future iteration.

---

## Open Questions

1. **`demux` multiple impls**: `demux` has 4 `StreamPeekRef` impls (one per output type). A single `#[output]` attribute can only target one field. Determine if all four are trivial or require manual handling; they may need to stay manual.

2. **`graph_node` two impls**: Similar to `demux` — two `StreamPeekRef` impls on the same struct. Evaluate case-by-case.

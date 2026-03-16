# Plan: Proc-macro derive for MutableNode and StreamPeekRef (Issue #124)

## Goal

Eliminate boilerplate in custom node definitions by introducing a `wingfoil-derive` proc-macro crate that generates `upstreams()` and `StreamPeekRef` impls from field attributes.

## New User-Facing API

```rust
#[derive(MutableNode)]
struct MyNode {
    #[active]  upstream_a: Rc<dyn Stream<Foo>>,   // triggers downstreams
    #[passive] upstream_b: Rc<dyn Stream<Bar>>,   // read-only, no trigger
    other_field: BTreeMap<K, V>,                  // ignored (non-stream)
    #[output]  value: Baz,                        // generates StreamPeekRef<Baz>
}

// Only cycle() is written manually — the only part that varies
impl MutableNode for MyNode {
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

## Step 2 — Implement `#[derive(MutableNode)]`

Entry point in `wingfoil-derive/src/lib.rs`:

```rust
#[proc_macro_derive(MutableNode, attributes(active, passive, output))]
pub fn derive_mutable_node(input: TokenStream) -> TokenStream { ... }
```

### Parse pass

Walk struct fields and classify each by its attributes:
- `#[active]`  → push `.as_node()` clone into `active` vec
- `#[passive]` → push `.as_node()` clone into `passive` vec
- `#[output]`  → record field name + type (used in Step 3)
- no attribute + type matches `Rc<dyn Stream<_>>` → **emit `compile_error!`** to prevent silent omissions

### Code generated for `upstreams()`

```rust
impl MutableNode for MyNode {
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(
            vec![self.upstream_a.clone().as_node()],
            vec![self.upstream_b.clone().as_node()],
        )
    }
    // cycle() is NOT generated — user always provides it
}
```

The partial impl only provides `upstreams()`; the user's manual `impl MutableNode` block provides `cycle()`. Rust merges them at compile time because `upstreams` has a default impl in the trait (to be confirmed — if not, the macro must generate a forwarding shim or the trait needs a default).

> **Architecture note:** If `MutableNode::upstreams` has no default, the derive must generate the full impl block containing only `upstreams`. The user's `impl MutableNode` block must then only provide `cycle`. Rust does not allow two `impl` blocks for the same trait on the same type, so the trait needs a default for `upstreams()` (returning empty), or the macro must generate a separate helper trait. **Preferred approach: add a `default fn upstreams()` on the trait returning `UpStreams::empty()`, then the derive overrides it.** If specialization is not available on stable, use a separate `Upstreams` trait and call it from `MutableNode::upstreams()` by default.

Simpler stable-Rust approach:
1. Add a blanket `UpstreamProvider` helper trait with `fn upstreams(&self) -> UpStreams`.
2. `MutableNode::upstreams()` delegates to `UpstreamProvider::upstreams()` by default.
3. `#[derive(MutableNode)]` generates `impl UpstreamProvider for MyNode { ... }`.
4. User only writes `impl MutableNode for MyNode { fn cycle(...) }`.

---

## Step 3 — Implement `#[derive(StreamPeekRef)]` (from `#[output]`)

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

Both derives can be combined into a single `#[derive(MutableNode)]` macro since they share the same attribute scan pass.

---

## Step 4 — Re-export from `wingfoil`

In `wingfoil/src/lib.rs`:

```rust
pub use wingfoil_derive::{MutableNode, StreamPeekRef};
```

Users need only `use wingfoil::*`.

---

## Step 5 — Migrate internal `MutableNode` impls (10 nodes)

Replace hand-written `upstreams()` with `#[derive(MutableNode)]` and field attributes:

| File | Struct |
|------|--------|
| `src/bencher.rs` | `BenchTriggerNode` |
| `src/nodes/always.rs` | `AlwaysTickNode` |
| `src/nodes/never.rs` | `NeverTickNode` |
| `src/nodes/node_flow.rs` | `ThrottleNode`, `DelayNode`, `LimitNode`, `FilterNode`, `FeedbackSendNode` |
| `src/nodes/tick.rs` | `TickNode` |

For nodes with non-`Rc<dyn Stream>` upstreams (e.g. `Rc<dyn Node>`), the derive must handle `Rc<dyn Node>` fields tagged `#[active]`/`#[passive]` as well.

---

## Step 6 — Migrate trivial `peek_ref` impls (38 nodes)

Replace `impl StreamPeekRef<T> for Foo { fn peek_ref(&self) -> &T { &self.value } }` with `#[output]` on the field and `#[derive(MutableNode)]` (or a standalone `#[derive(StreamPeekRef)]`).

Full list from issue: `average`, `bimap`, `buffer`, `callback`, `channel`, `combine`, `constant`, `delay`, `delay_with_reset`, `demux` (4 impls), `difference`, `distinct`, `feedback`, `filter`, `fold`, `graph_node` (2), `graph_state`, `inspect`, `limit`, `map`, `map_filter`, `merge`, `print`, `producer`, `sample`, `throttle`, `trimap`, `try_bimap`, `try_map`, `try_trimap`, `window`, `iterator_stream` (2), `async_io`.

Nodes with non-trivial `peek_ref` (delegation to an inner stream) are evaluated case-by-case and left as manual impls.

---

## Step 7 — Update docs and examples

- Update `examples/dynamic/dynamism/` to use the derive
- Update custom-node authorship guide in `CONTRIBUTING.md` or `README.md`
- Add a minimal example in `wingfoil-derive/README.md`

---

## Acceptance Criteria (from issue)

- [ ] `wingfoil-derive` crate compiles with `#[derive(MutableNode)]`
- [ ] `upstreams()` correctly separates `#[active]` and `#[passive]` fields
- [ ] `StreamPeekRef` is derived from `#[output]`
- [ ] Compile error (not silent bug) if an `Rc<dyn Stream<T>>` field has no attribute
- [ ] All 10 internal `MutableNode` impls migrated
- [ ] All trivial `peek_ref` impls migrated (38 nodes)
- [ ] `examples/dynamic/dynamism/` updated
- [ ] Existing tests pass unchanged

---

## Open Questions

1. **Stable specialization**: Confirm whether `MutableNode::upstreams` currently has a default impl. If not, decide between the helper-trait approach (preferred) or requiring users to omit `upstreams()` from their manual impl block entirely (macro generates the full trait impl).

2. **`Rc<dyn Node>` vs `Rc<dyn Stream<T>>`**: Upstream fields may be typed as `Rc<dyn Node>`. The attribute detection heuristic should accept both; the compile-error lint for un-attributed stream fields should recognise both types.

3. **`demux` multiple impls**: `demux` has 4 `StreamPeekRef` impls. Determine if all are trivial or require manual handling.

4. **Derive naming**: Use a single `#[derive(MutableNode)]` that handles both `upstreams` and `StreamPeekRef`, or two separate derives (`MutableNode` + `StreamPeekRef`)? Separate derives are more composable — prefer that.

# Plan: `#[node]` attribute macro for MutableNode (Issue #124)

## Goal

Eliminate boilerplate in custom node definitions by introducing a `wingfoil-derive` proc-macro
crate that generates `upstreams()` and `StreamPeekRef` impls from a single attribute on the
`impl MutableNode` block.

## New User-Facing API

```rust
pub struct MapStream<IN, OUT: Element> {
    upstream: Rc<dyn Stream<IN>>,
    value: OUT,
    func: Box<dyn Fn(IN) -> OUT>,
}

#[node(active = [upstream], output = value: OUT)]
impl<IN, OUT: Element> MutableNode for MapStream<IN, OUT> {
    fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
        self.value = (self.func)(self.upstream.peek_value());
        Ok(true)
    }
}
```

The `#[node]` attribute:
- Injects `fn upstreams()` into the impl block when `active`/`passive` fields are specified
- Emits a separate `impl StreamPeekRef<T>` when `output = field: Type` is specified
- Neither → no-op (source node uses the `UpStreams::none()` default on `MutableNode`)

---

## Design

### Why attribute macro on the impl block (not struct-level derive)?

Rust allows only one `impl Trait for Type` block. A struct-level `#[derive]` would have to
generate a *separate* impl — requiring `upstreams()` to live in a supertrait (`WiringPoint`) so
the derive's impl doesn't conflict with the user's `impl MutableNode`.

An attribute macro *transforms* the existing impl block in-place, injecting `upstreams()` directly
into the user's `impl MutableNode`. No supertrait split, no extra trait in the public API.

### `MutableNode` changes

`upstreams()` is back in `MutableNode` with a default of `UpStreams::none()`:

```rust
pub trait MutableNode {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool>;
    fn upstreams(&self) -> UpStreams { UpStreams::none() }  // default: source node
    // ...setup, start, stop, teardown (all defaulted)
}
```

### `AsUpstreamNodes` helper trait

Retained in `types.rs` — used by the generated `upstreams()` body to handle
`Rc<dyn Node>`, `Rc<dyn Stream<T>>`, and `Vec<U: AsUpstreamNodes>` uniformly.

### Attribute syntax

| Pattern | Annotation |
|---------|------------|
| Transform (active upstream, stream output) | `#[node(active = [upstream], output = value: OUT)]` |
| Sink (active upstream, no output) | `#[node(active = [upstream])]` |
| Source with output | `#[node(output = value: T)]` |
| Mixed passive/active | `#[node(passive = [data], active = [trigger], output = value: T)]` |
| Complex upstreams (`Dep<T>`, optional, etc.) | Write `upstreams()` manually in impl block; use `#[node(output = ...)]` for StreamPeekRef only |
| Source, no output | No annotation (default `UpStreams::none()` applies) |

---

## Completed Work

- `wingfoil-derive` crate with `#[proc_macro_attribute] fn node(...)`
- `MutableNode`: `upstreams()` restored with default `UpStreams::none()`; `WiringPoint` supertrait removed
- `AsUpstreamNodes` helper trait retained in `types.rs`
- All nodes in `wingfoil/src/nodes/`, `wingfoil/src/adapters/`, `wingfoil/src/bencher.rs`,
  `wingfoil-python/src/proxy_stream.rs`, `wingfoil/src/graph.rs` (test nodes), and
  `wingfoil/examples/dynamic/dynamic-manual/main.rs` migrated

---

## Acceptance Criteria

- [x] `#[node(active = [...], output = field: Type)]` generates correct `upstreams()` and `StreamPeekRef<T>`
- [x] `MutableNode` has `upstreams()` with default `UpStreams::none()`; no `WiringPoint` supertrait
- [x] `AsUpstreamNodes` handles `Rc<dyn Node>`, `Rc<dyn Stream<T>>`, `Vec<...>`
- [x] All nodes migrated
- [x] All 94 unit tests pass
- [x] `cargo clippy --workspace --all-targets --all-features` — no errors

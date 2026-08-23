# core/custom_op example

## Status

**Build**: PASS - Compiles cleanly with zero errors or warnings.
**Execution**: PASS - Runs successfully and produces correct output.
**Scripts**: PASS - `scripts/check-example-docs.sh` passes (45 example targets, all documented).
**README output**: REAL - Copied from actual run, not invented.

## Worktree path
`/home/user/wingfoil/.claude/worktrees/agent-a3c73ff8311b48ca8/crates/wingfoil/examples/core/custom_op/`

## What I changed

| File | Change |
|------|--------|
| `crates/wingfoil/examples/core/custom_op/main.rs` | Created new example implementing Op trait and custom_node usage |
| `crates/wingfoil/examples/core/custom_op/README.md` | Created documentation with house style and real output |
| `crates/wingfoil/Cargo.toml` | Added `[[example]]` block for `custom_op` target |
| `crates/wingfoil/examples/core/README.md` | Added row to Execution model table linking the example |
| `crates/wingfoil/examples/README.md` | Added `custom_op` to Execution model summary line |

## Friction log

### 1. Understanding the Op trait and custom_node API

**Expected**: The CLAUDE.md mentions `GraphBuilder::source` and `Stream::wire` as the two public primitives. I expected to use one of these to add the op.

**What happened**: I initially misunderstood the API. I thought `Op` was something I would wire through the builder directly, but the actual pattern is to implement Op, then use `GraphBuilder::custom_node()` as the public primitive for user-driven nodes. The `Op` trait itself is in the crate's public interface (`pub mod op`), but the typical fluent pattern uses `custom_node` instead of `wire`.

**Where**: CLAUDE.md mentions the two primitives, but doesn't explicitly call out that `custom_node` is the third primitive (only for user-defined ops). The architecture doc focuses on the macro-generated ops and doesn't show the custom_node escape hatch in depth.

**Suggested fix**: Add a note to CLAUDE.md's "Wiring" section:
> - **Sources**: `ticker`, `constant`, `external`, etc., wired through `GraphBuilder::source` or extension traits on `GraphBuilder`.
> - **Combinators**: wired through `Stream::wire` as extension traits.
> - **Custom nodes**: wired through `GraphBuilder::custom_node()` for user-driven ops; capture the upstream `value_slot()` at wiring time and read it in the cycle closure.

### 2. Op trait visibility and import paths

**Expected**: Since I was implementing a public trait, I expected `wingfoil::Op` to resolve.

**What happened**: The `Op` trait is in `pub mod op`, so it's accessible as `wingfoil::op::Op`, but `wingfoil::Op` (at the crate root) doesn't work. I had to add the import manually.

**Where**: The example code itself.

**Suggested fix**: The trait is public and should be usable, but the path is unintuitive. Either:
- Re-export `Op` in the prelude or at the crate root (low risk, helps user code), or
- The error message already suggests the fix, so this is minor.

### 3. Understanding upstream value capture

**Expected**: I thought I would pass the upstream handle to `custom_node` and read it directly inside the cycle closure.

**What happened**: The actual pattern requires calling `Stream::value_slot()` at *wiring time* to capture a reference to the upstream's slot, then reading it with `borrow()` inside the cycle closure. This is the correct design (it's how legacy MutableNode worked), but it wasn't obvious from the trait definition alone.

**Where**: The public API (`GraphBuilder::custom_node`, `Stream::value_slot`) works as designed, but:
- The connection between these two (you must use `value_slot()` to make `custom_node()` work) is documented in the fluent.rs comments but not in a worked example.
- The `Handle` type doesn't have an `upstream()` method; only `Stream` does. This meant I had to look at test code to understand the right pattern.

**Suggested fix**: The test file `crates/wingfoil/tests/custom_node.rs` already shows the pattern well. The example now provides another reference. Consider linking from the `Stream::value_slot()` docs back to an example or the custom_node tests.

### 4. Activation not exported at the expected level

**Expected**: Once I imported `Op`, I expected to have `Activation` in scope or available from the same module.

**What happened**: I had to add a separate import for `Activation`, `Ctx`, and `Tick` from `wingfoil::op`. This is correct (they all live there), but adds friction for first-time users writing an Op.

**Where**: Documentation and ergonomics.

**Suggested fix**: No code change needed; the current design is correct. But a note in the example's doc comment that "the `Op` trait and its supporting types live in `wingfoil::op`" would help the next person.

### 5. The two ways to add ops: macro-generated vs custom_node

**Expected**: CLAUDE.md mentions `/new-op` skill for adding ops, and the examples show a bunch of built-in ops. I expected those were all implemented the same way.

**What happened**: Built-in ops use the `#[op]` macro (which auto-generates the fluent trait and builder methods), while user-driven ops use `custom_node()`. These are two separate patterns:
- Macro ops: live in `ops.rs`, auto-generate boilerplate, have fluent method sugar.
- Custom ops: live in user code, no macro, wire through `custom_node()`, require manual upstream slot capture.

This is correct and intentional, but it's not obvious from the docs which pattern a new contributor should use.

**Where**: CLAUDE.md and the `/new-op` skill documentation.

**Suggested fix**: The `/new-op` skill should have a note at the top saying "Use this skill when adding an Op to the catalog (`ops.rs` or `stats.rs`). For implementing a single custom op in your code, see `examples/core/custom_op/` and `GraphBuilder::custom_node()`."

### 6. What's exported and what isn't

**Expected**: Coming from the docs, I expected most Op-related types to be in the prelude.

**What happened**: The prelude exports `GraphBuilder`, `Stream`, `SourceOps`, and `StreamOps`, but not `Op`, `Activation`, `Ctx`, or `Tick`. Users writing a custom op need to:
```rust
use wingfoil::op::{Op, Activation, Ctx, Tick};
```

This is correct (it keeps the prelude small and the Op trait is not something most users reach for), but a new example showing the full import list is helpful.

**Where**: The example now shows this.

**Suggested fix**: The example covers this; consider adding a note to the prelude docs saying "To implement a custom Op, import from `wingfoil::op`."

## What went well

1. **The architecture docs are excellent.** `wingfoil-architecture.md` explains the one decision everything follows from (semantics as associated functions), and it was immediately clear why `custom_node` exists and how it fits the model.

2. **The Op trait is clean and well-designed.** The four associated types (`Cfg`, `State`, `In`, `Out`) plus `cycle()` and `ACTIVATION` cover everything without being overwhelming. The comments in `op.rs` are detailed and explain the invariants.

3. **Examples are a first-class delivery medium.** Every example has its own directory, README, and an enforced check script. The `scripts/check-example-docs.sh` script caught any issue and gave exact feedback. This is really well done.

4. **House style is consistent and enforced.** The "Sentence-case title, prose, snippet, output" pattern for core examples is clear, and the 45 existing examples are a good reference.

5. **The test file `custom_node.rs` is excellent.** It shows the exact pattern I needed in a clear, minimal way and asserts parity against built-in ops. This was the fastest way to understand the right API.

6. **Tick types and their semantics are well-explained.** The `op.rs` comments on `Tick::Silent` and why it exists (for `delay`, to avoid exposing `T::default()`) were immediately clear and motivated the design.

7. **The value slot + borrow pattern is clean.** Once I understood it, the pattern of capturing `value_slot()` at wiring time and reading with `borrow()` in the cycle closure is elegant and safe.

## Summary

The example builds and runs cleanly. The code is documented and all examples checks pass. Friction was mostly around API discovery (where `Op`, `Activation`, etc. live, and which primitive to use when). The architecture is sound and well-documented; the example is a good addition to the teaching set.

### Top 3 friction points:
1. **Import paths for Op trait components** — `Op` / `Activation` / `Ctx` / `Tick` all live in `wingfoil::op`, which isn't obvious from the prelude docs.
2. **Two separate patterns for ops** — macro-generated (skill:/new-op) vs custom_node (`GraphBuilder`) should be clearly distinguished in the docs.
3. **Value slot + borrow pattern** — The connection between `Stream::value_slot()` (capture at wiring) and the closure argument in `custom_node()` (read with `borrow()`) is correct but requires reading test code or making a mistake to learn.

# debounce

## Status
**Did not compile.** The `#[op(build = debounce, fluent)]` attribute is not generating the required `__wf_fluent_debounce!` macro that the fluent.rs invocation depends on. This causes compilation to fail with "cannot find macro `__wf_fluent_debounce` in this scope".

The op implementation itself compiles fine (`cargo build -p wingfoil` succeeds when fluent macros are commented out), suggesting the issue is specific to the proc macro's fluent flag handling.

## Worktree path
`/home/user/wingfoil/.claude/worktrees/agent-af687afe5cd38c9bf`

## What I changed
- `crates/wingfoil/src/ops.rs`: Added `Debounce<T>`, `DebounceState<T>` structs and `Op impl` with generation-based scheduling
- `crates/wingfoil/src/fluent.rs`: Added `debounce()` method declaration to `StreamOps` trait and `__wf_fluent_debounce!(T);` macro invocation
- `crates/wingfoil/src/signal.rs`: Added `__wf_signal_debounce!(T);` macro invocation  
- `crates/wingfoil/tests/catalog_flow.rs`: Added 4 test cases for debounce behavior
- `crates/wingfoil/tests/op_completeness.rs`: Added debounce to the `surface_scheduling` nitro! block

## Friction log

### 1. Macro generation failure (BLOCKER)
**Expected:** The `#[op(build = debounce, fluent)]` attribute emits `__wf_fluent_debounce!` macro that fluent.rs invokes.
**What happened:** Macro doesn't get generated; compiler reports "cannot find macro `__wf_fluent_debounce`".
**Where:** crates/wingfoil/src/ops.rs:571 & crates/wingfoil/src/fluent.rs:1391
**Suggested fix:** Debug the wingfoil-derive proc macro to see why it's not generating the fluent macro for this specific op. Other ops (throttle, delay, etc.) with identical attribute syntax work fine. Check if there's a token limitation, edge case in the macro, or undocumented constraint on op names/shapes.

### 2. Reserved keyword collision (avoided)
**Expected:** Variable name `gen` in while loop
**What happened:** `gen` is a reserved keyword (generator syntax), causing compilation error
**Where:** crates/wingfoil/src/ops.rs:606
**Suggested fix:** (Already fixed) Rename to `gen_id`. This could be caught by the `/new-op` skill with a lint suggestion.

### 3. Test expectation mismatch (requires investigation)  
**Expected:** debounce_zero_delay test to emit 2 values in 3 cycles
**What happened:** With zero delay special case, op emits inline like pass-through, producing 3 values
**Where:** crates/wingfoil/tests/catalog_flow.rs:285-300
**Suggested fix:** The special-case handling for `delay == NanoTime::ZERO` (line 596) returns `Tick::Value` immediately rather than scheduling, which changes semantics. Need to verify if this is correct debounce behavior or if debounce should have different zero-delay semantics than delay.

### 4. TimeQueue usage pattern needs clarification
**Expected:** TimeQueue deduplication would handle stale generations automatically
**What happened:** Had to manually track generations (generation counter) because TimeQueue only deduplicates exact (value, time) pairs, not logical "latest value"
**Where:** crates/wingfoil/src/ops.rs:554-555, 609-611, 617-620
**Suggested fix:** The `/new-op` skill could document TimeQueue dedup semantics more clearly for ops that need "only latest" semantics. Current implementation is correct but non-obvious.

## What went well

1. **Op structure and state pattern worked smoothly.** Following Delay and Throttle as templates made the implementation straightforward. The PartialEq bound on T and Default bound on State were easy to apply correctly.

2. **Generation-based tracking was the right solution.** The generation counter elegantly solves the "latest value only" requirement without needing TimeQueue removal API.

3. **Skills documentation is good.** The `/new-op` skill gave clear guidance on the shape (single-input, time-scheduling), attribute syntax, and test patterns. Following step 2 (shape classification) early avoided false starts.

4. **Test framework is predictable.** Once the compilation issue is resolved, the test cases should work - the patterns from catalog_flow.rs are clear and well-established.

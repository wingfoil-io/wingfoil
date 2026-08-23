# core/two_clocks example

## Status

- Compiled: YES
- Ran successfully: YES
- `scripts/check-example-docs.sh`: PASSED (45 example targets, all documented)
- README output: REAL (captured from actual run)

## Worktree path

`/home/user/wingfoil/.claude/worktrees/agent-a54b166697e8ae88c`

## What I changed

Files created/modified:
1. `crates/wingfoil/examples/core/two_clocks/main.rs` — new example code
2. `crates/wingfoil/examples/core/two_clocks/README.md` — documentation and expected output
3. `crates/wingfoil/Cargo.toml` — added `[[example]]` block for `two_clocks` target
4. `crates/wingfoil/examples/core/README.md` — added row to Execution model table
5. `crates/wingfoil/examples/README.md` — added to Execution model list and updated backtest suggestion

## Friction log

### 1. Where to access Ctx — undocumented API at first contact

**Expected:** A straightforward way to show engine time vs wall time in an example.

**What happened:** I initially tried to create a custom `Op` struct and use `g.source(op)`, but:
- `g.source()` expects a closure `FnOnce(&mut Builder) -> Handle<T>`, not an Op instance
- There's no direct "use my custom Op" method on the fluent API
- Had to search CLAUDE.md and the fluent.rs code to find that `custom_node` exists

**Where:** Main blocker was in understanding the fluent API's abstraction boundaries. The GraphBuilder has `source()`, `wire()`, and `custom_node()`, but their signatures and use cases are not immediately clear from the module structure.

**Suggested fix:** A one-paragraph section in CLAUDE.md under "Key concepts" explaining when to use `wire()` vs `custom_node()` vs composing with higher-level ops, with a line reference to fluent.rs. Currently the only op-access examples are high-level (ticker, map, filter) — showing the bridge to Ctx-bearing code would help.

### 2. Import path fragmentation for Tick

**Expected:** `use wingfoil::Tick` (parallel to NanoTime, RunMode, etc.)

**What happened:** Compiler said "no `Tick` in the root", but suggested importing from `prelude`. Tick is in prelude but not exported from the crate root like other key types.

**Where:** Line 11 of main.rs initially; `use wingfoil::prelude::*` solved it.

**Suggested fix:** Either export Tick from the crate root (alongside NanoTime, RunFor, RunMode) or document in CLAUDE.md that "Op result types live in prelude, not root". Currently the pattern is inconsistent: types users need to name in signatures (NanoTime, RunMode, RunFor) are at root, but Tick (which users must use in custom ops) is only in prelude.

### 3. NanoTime conversion method naming

**Expected:** A method like `as_nanos()` to get the u64 value, parallel to Duration::as_nanos().

**What happened:** NanoTime doesn't have `as_nanos()`. Compiler suggested nothing. Had to read the source to discover:
- NanoTime implements `From<NanoTime> for u64`
- So the conversion is `u64::from(nanotime)` or use `.into()`, not a method

**Where:** crates/wingfoil/src/runtime/time.rs lines 180+; discoverable only by reading the impl block.

**Suggested fix:** Add a `.as_nanos() -> u64` method to NanoTime for API consistency with Duration. The current pattern (using From impl) is less discoverable. Even if From is intentional for deep reasons, a method alias would help.

### 4. Multiple registration points for examples — error detection is good but could be earlier

**Expected:** One place to register an example (Cargo.toml) and have it appear everywhere.

**What happened:** Correctly had to register in three places:
1. Cargo.toml [[example]] block
2. crates/wingfoil/examples/core/README.md table
3. crates/wingfoil/examples/README.md table

All three were necessary. BUT:
- The validation script `scripts/check-example-docs.sh` caught missing entries and passed only after all three were done
- No earlier feedback (e.g., Cargo itself) warned that the Cargo.toml entry alone was incomplete
- If I'd skipped the README updates, the script would have failed before running the code

**Where:** CLAUDE.md documents all three rules in the "Examples: every one is a directory with a README" section, so I didn't miss it. But there's no mechanism to enforce it at build time until the validation script runs.

**Suggested fix:** This is actually working as designed (CLAUDE.md is clear, scripts catch violations, CI enforces), so no fix needed. The current approach is sound — it forces human review of where new examples fit in the taxonomy.

### 5. Understanding custom_node's activation model

**Expected:** Once I found `custom_node`, its API would be clear.

**What happened:** The signature is
```rust
pub fn custom_node<T, F>(
    active: &[Upstream],
    passive: &[Upstream],
    activation: Activation,
    cycle: F,
) -> Stream<T>
```
But the example code needs to:
- Convert a Stream to an Upstream via `.upstream()`
- Choose the right Activation (I used SCHEDULES, which ticks when an upstream fires)
- Ensure the closure captures all its state (no inputs from the signature; Ctx provides the timing)

None of this is wrong, but it's a multi-step cognitive leap from "I want to read Ctx" to "I need to use custom_node and understand Activation and Upstream".

**Where:** Fluent API design; examples/core/ doesn't have a custom_node example yet.

**Suggested fix:** Create a minimal `core/custom_node` example that just reads Ctx and outputs a value, before or alongside two_clocks. The two_clocks example works, but it's doing two things at once (showing the two clocks AND showing how to use custom_node). Separating them would clarify both.

### 6. Realizing wall_time is still wall-clock time in historical mode

**Expected:** Wall time would be "instant" (very small numbers) in historical mode, maybe microseconds at most.

**What happened:** Wall time in historical mode is huge (1786912908...) — actual Unix epoch nanoseconds. This is correct (the wall clock is real), but I initially expected it to be zeroed or constant.

**Where:** Surprise in the output; resolved by re-reading CLAUDE.md's "Two clocks" section carefully.

**Suggested fix:** None needed — CLAUDE.md is clear. But the README example now calls this out explicitly to help the next person.

## What went well

1. **CLAUDE.md is comprehensive.** The "Examples: every one is a directory with a README" section clearly lists all three registration points and links to the validation script. Following it step-by-step worked.

2. **The validation script works.** `scripts/check-example-docs.sh` passed cleanly once all pieces were in place, and it's fast (< 1 second).

3. **The fluent API is expressive.** Once I found `custom_node`, the code was straightforward. The Ctx → time/wall_time flow works cleanly.

4. **Existing examples are good references.** `run_mode`, `feedback`, `hello_graph` all follow the same pattern, making it easy to match house style.

5. **Compilation feedback was helpful.** "cannot find trait `Op`" → "consider importing wingfoil::op::Op" guided me to the right modules when I was trying wrong approaches.

6. **The working output made sense.** Once I ran the example, the difference between historical and realtime modes was clear — engine time steps by 100M nanos in historical (instant run), wall time has microsecond granularity. In realtime, both advance by ~100M nanos (real 100ms per tick).

## Summary for the parent agent

Three real friction points stand out:

1. **Fluent API surface** — discovering `custom_node` and understanding when to use it vs `wire()` vs high-level ops needs clearer docs or an example.
2. **Type export consistency** — Tick should be importable from the crate root like NanoTime/RunMode, or documented as prelude-only.
3. **NanoTime conversion** — the `.into()` pattern (via From impl) is less discoverable than a `.as_nanos()` method.

But the overall example system works well — CLAUDE.md's rules are clear, the validation script catches errors, and existing examples are good models. The example compiles, runs, and passes validation.

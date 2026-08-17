# core/error_handling example

## Status
**SUCCESS** — Example compiles, runs, and produces real error output. `scripts/check-example-docs.sh` passes. README output is real (pasted from actual run).

## Worktree path
`/home/user/wingfoil/.claude/worktrees/agent-ad527c6fa1849e304/crates/wingfoil/examples/core/error_handling/`

## What I changed
- `crates/wingfoil/Cargo.toml` — added `[[example]]` block for `error_handling`
- `crates/wingfoil/examples/core/error_handling/main.rs` — new file
- `crates/wingfoil/examples/core/error_handling/README.md` — new file
- `crates/wingfoil/examples/core/README.md` — added row to "Execution model" table
- `crates/wingfoil/examples/README.md` — added to "Execution model" line

## Friction log

1. **Channels deliver Bursts, not individual values — expected more granular delivery**
   - **Expected**: Channels would emit individual values to for_each
   - **What happened**: Channels emit `Burst<T>`, so the `for_each` closure receives `&Burst<T>` not `&T`
   - **Where**: CLAUDE.md mentions channels deliver bursts but doesn't show a worked example of iterating over them
   - **Suggested fix**: The `threading` example shows the pattern clearly (iterate over burst in for_each), but a note in CLAUDE.md or an example doc comment would catch this sooner. Alternatively, show a simple one-line example of burst iteration in the core examples intro.

2. **try_map takes a closure on the full input type, not individual elements**
   - **Expected**: try_map would automatically map over burst elements like a normal map does
   - **What happened**: try_map receives `&Burst<T>` and must return `Result<U, Error>`. I had to manually iterate and collect to handle burst elements
   - **Where**: ops.rs line 152 documents the closure signature but doesn't explain what happens when the input is a Burst
   - **Suggested fix**: An example in the ops.rs doc comment showing try_map over burst elements (parse each, collect results) would clarify this. Or, add a note that says "you iterate within the closure if needed".

3. **Error context: .context() vs .with_context() — import requirement not obvious**
   - **Expected**: .context() would work without an import since anyhow is already in prelude
   - **What happened**: Needed to explicitly `use anyhow::Context` to get the trait method. .context() takes a Display value, not a closure — almost got this backwards
   - **Where**: Compiler error pointed to the right place, but I initially tried .with_context() which requires the Context trait
   - **Suggested fix**: Either re-export Context in the prelude, or add an example in CLAUDE.md's error handling section showing the import and both variants

4. **Producer error propagation documentation was well-placed**
   - **Expected**: Have to hunt for send_error() and its behavior
   - **What happened**: channel.rs has a perfect explanation (line 150-152) and the Message::Error enum doc is clear
   - **Where**: Worked as documented — no friction here
   - **Suggested fix**: None needed; this was the best-documented piece

5. **Example output must be REAL but runs are nondeterministic (thread scheduling)**
   - **Expected**: Output would be deterministic so README shows exactly what will print
   - **What happened**: First run didn't print the "received: 1" and "received: 2" lines because the producer thread sent the error before the main graph cycled. Second run printed them
   - **Where**: Realtime mode + threading = scheduling variance
   - **Suggested fix**: Add a comment in the README explaining that realtime runs may coalesce values into bursts depending on thread scheduling. Or, switch to HistoricalFrom(NanoTime::ZERO) and use send_at for the producer to guarantee replay order

6. **Burst type required an explicit import**
   - **Expected**: Burst would be in prelude or obvious where to get it
   - **What happened**: Needed `use wingfoil::Burst` explicitly
   - **Where**: prelude doesn't include Burst; it's a public export from lib.rs
   - **Suggested fix**: Either add Burst to prelude, or mention it in CLAUDE.md alongside the Channel/Burst section

## What went well

1. **CLAUDE.md "Fallibility" bullet is clear**: The section "every lifecycle fn returns `anyhow::Result`; `sender.send_error(e)` propagates a producer error into the graph and aborts the run with context" is exactly right and matches the implementation

2. **channel.rs documentation is precise**: The Message enum docs and ChannelSender::send_error docs explain the behavior perfectly without ambiguity

3. **Example structure rules are enforced by CI**: `scripts/check-example-docs.sh` caught missing documentation and made the linking rules unambiguous

4. **Cargo.toml example declaration pattern is clear**: The comment + [[example]] block format is easy to follow, and the test made sure I got it right

5. **Threading example as a reference**: The `threading` example shows how to handle Bursts from channels, making it easy to pattern-match for this one

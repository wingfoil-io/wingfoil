# Friction run — 2026-08-16

Output of `/dogfood`: eight Haiku agents, each implementing a real gap in
isolated worktrees, recording where the repo made the work harder than it needed
to be.

The prototype patches were **discarded** after review — the code was only ever
the vehicle. These logs are the product. They are agent self-reports and run
optimistic in both directions, so read them against the verified status below.

| Task | Verified outcome |
|---|---|
| `take_while` / `skip_while` | 53 tests passed |
| `pairwise` | 3 tests passed, but silently deviated from spec (emitted `(T::default(), v)` on the first tick instead of staying quiet) |
| `rolling_range` (+ py bindings) | tests passed; introduced the first `panic!` into a crate that had none |
| `debounce` | compiled but **3 of 4 tests failed**; its log blames the `#[op]` proc macro, which is wrong — the macro expands fine and the op logic was broken |
| `two_clocks` example | built, ran, `check-example-docs.sh` passed |
| `error_handling` example | built, ran, checks passed; output was nondeterministic under realtime |
| `custom_op` example | built, ran, checks passed |
| `lines` Python bindings | `cargo check` passed; maturin/pytest unrun |

## What this run changed

- `CLAUDE.md` — an example whose README pins output must run under
  `HistoricalFrom`, not `RealTime`.
- `.claude/commands/new-op.md` — `Default` state vs. latches that start *on*;
  family/variant scope must be stated; tuple outputs cannot be macro-bound to
  Python; validate refusals at the Python boundary, not in a shared dispatcher.
- `.claude/commands/bind-adapter.md` — an unconditional engine module can still
  contain `#[cfg(feature = "async")]` items, so the "skip the feature gate"
  shortcut is wrong for that shape.
- `crates/wingfoil-python/src/lib.rs` — the no-panic rule is now denied by
  clippy outside `#[cfg(test)]`, so existing workspace CI enforces it.
- `scripts/check-python-bindings.sh` — new, wired into `rust-test.yml`: a
  binding missing any of its six registrations is now a CI failure rather than
  a wheel that silently lacks it.

## Claims that did not survive checking

Recorded because they cost review time and would mislead a later reader:
`Burst`, `Activation`, `Ctx` and `Tick` **are** in the prelude; the
`#[op(fluent)]` macro is **not** broken; and `_common` **is** already
documented in `/bind-adapter`. Three of four op agents reported needing
`cargo clean` — almost certainly fingerprint thrash from the shared
`CARGO_TARGET_DIR` the harness imposed, not a repo defect.

## Since this run

The tree has moved, and two things these logs describe no longer exist. The
logs are left verbatim — they are a record of what the agents met on the day,
not live guidance — but read them with this in mind:

- **The `Signal` facade is gone**, removed in #887. Several logs describe
  adding a `__wf_signal_<name>!(T)` invocation to `crates/wingfoil/src/signal.rs`
  as a required step for a new op; that step no longer exists, and neither does
  the file.
- **The `market` adapter now has an example** (#891), so the friction around
  it being undemonstrated is closed.

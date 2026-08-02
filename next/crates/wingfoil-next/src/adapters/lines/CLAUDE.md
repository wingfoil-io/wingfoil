# lines Adapter (wingfoil-next)

A line-oriented file adapter: newline-delimited `String` records in and out,
no serde. It is the **smallest complete Op-pattern I/O edge** in the tree and
the reference every other adapter's shape is explained against.

**Next-only — classic has no `lines` adapter.** There is no parity oracle;
instead, `lines` is the thing that keeps the *conventions* honest, so changes
here ripple into `/new-adapter-next` and into the other adapters' docs.

## Layout

```
adapters/
  lines.rs          # the whole adapter
  lines/CLAUDE.md   # this file
```

## Feature gating

The module is declared **unconditionally** (`pub mod lines;` — no `#[cfg]`).
Within it:

- `tail_lines` and the sink are **dependency-free** — they compile in a default
  build with no features at all.
- `replay_lines` / `replay_lines_scheduled` are `#[cfg(feature = "async")]`:
  they ride the lazy, bounded `produce_async` producer (register B4).

## Entry points

| Item | Kind | Notes |
|---|---|---|
| `replay_lines(g, path, buffer_size)` | source | historical replay, one line per nanosecond from `NanoTime::ZERO`; `async` |
| `replay_lines_scheduled(g, path, base, step, buffer_size)` | source | same, with a caller-chosen `base`/`step`; `async` |
| `tail_lines(g, path)` | source | realtime busy-spin `poll` tail; dependency-free |
| `LinesSinkOps::write_lines` / `append_lines` | sink trait on `Stream<Burst<T>>` | truncate / append; `T: Display` |

All sources emit `Stream<Burst<String>>`; both sinks return `Result<Stream<()>>`
(the file is opened at wiring, so a bad path fails before the run).

## What to know before changing it

- **Burst grouping is the point.** With the default `step` of 1 ns every record
  lands at a distinct instant, so each burst carries one line; a zero `step`
  (or a `base`/`step` that collides) groups records into one atomic burst.
  Tests assert both shapes — don't "simplify" the stamping.
- **`buffer_size` is real back-pressure in both run modes** (register B5). The
  file is *not* read up front: lines are pulled as the graph drains, a
  same-time group of any size riding one slot. Do not reintroduce an eager
  `replay_results` collection.
- **`tail_lines` busy-spins.** It is a `g.poll` source, so the kernel never
  parks while it exists — realtime only, one core pinned. Its line
  reassembly is factored into the free function `poll_line` precisely so it
  can be unit-tested without a realtime run; keep it that way.
- **There is deliberately no `Stream<T>` convenience sink impl.** `Burst<T>` is
  a `tinyvec` that implements `Display`, so a `Stream<Burst<T>>` *is* a
  `Stream<T: Display>` and a second impl becomes ambiguous (E0283) or silently
  shadows the burst form, writing `[ALPHA]` instead of `ALPHA`. `csv` can offer
  both because its bound is `Serialize`. This is written up in
  `/new-adapter-next` step 8 — leave the trait burst-only.
- Sinks use `for_each_mut`, not a hand-rolled `RefCell`-in-a-`Fn` dance.

## Deviations from classic

None to record — there is no classic `lines`. The module `//!` header is the
canonical description of behaviour.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/lines_adapter.rs` | `#![cfg(feature = "async")]` | nothing |

```bash
cargo test -p wingfoil-next --features async --test lines_adapter
```

No integration tier and no dedicated workflow — it is a file adapter (skill
step 10, Option C). It runs in `rust-test.yml`'s `test-next` job with the rest.

`tests/lines_adapter.rs` is also the **reference for unique temp paths** (pid +
atomic counter) that `next/CLAUDE.md` points every adapter test at.

## Example

`examples/lines_adapter.rs`, `required-features = ["async"]`.

```bash
cargo run -p wingfoil-next --features async --example lines_adapter
```

## Python

**No binding.** `lines` is not in `wingfoil-next-python` — it is a
demonstration adapter, and the Python surface has `csv` for file replay. If one
is ever added, run `/bind-adapter-next lines`.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test -p wingfoil-next --features async
```

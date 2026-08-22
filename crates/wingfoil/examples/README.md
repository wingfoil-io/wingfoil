# Wingfoil Examples

Every example is runnable, lives in its own directory, and has a README
explaining what it teaches, the wiring, and its expected output.

```sh
cargo run -p wingfoil --example <name>                      # core examples
cargo run -p wingfoil --example <name> --features <feature> # anything gated
```

## New here? Read these three

| # | Example | Command |
|---|---|---|
| 1 | [**`hello_graph`**](core/hello_graph/) — wire → build → run, historical and realtime | `cargo run -p wingfoil --example hello_graph` |
| 2 | [**`ema_crossover`**](core/ema_crossover/) — `fold`/`join`/`map`/`filter` at backtest scale | `cargo run -p wingfoil --example ema_crossover` |
| 3 | [**`order_book`**](core/order_book/) — real market data in and out: a CSV of AAPL limit orders, a book, trades and two-way prices | `cargo run -p wingfoil --features csv --example order_book` |

Then pick a direction:

- want to plug in **real data**? → [`adapters/`](adapters/), starting with
  [`lines`](adapters/lines/) or [`csv`](adapters/csv/) (no server needed).
- want to know **how fast**? → [`core/topological_sort`](core/topological_sort/), then
  [`core/dual_mode`](core/dual_mode/), then [`showcase/`](showcase/).
- want to **backtest**? → [`core/run_mode`](core/run_mode/).

## The three groups

| Group | What's in it | Needs |
|---|---|---|
| [**`core/`**](core/) | Engine concepts — wiring, run modes, execution tiers, threading, dynamism | Nothing |
| [**`adapters/`**](adapters/) | One directory per I/O adapter — files, brokers, stores, protocols, telemetry | A feature flag; some need a server |
| [**`showcase/`**](showcase/) | Multi-process end-to-end latency demonstrations | `--release`, several services |

### Core — [full index](core/)

**Start**: [`hello_graph`](core/hello_graph/) · [`ema_crossover`](core/ema_crossover/) · [`order_book`](core/order_book/)

**Execution model**: [`run_mode`](core/run_mode/) · [`topological_sort`](core/topological_sort/) · [`feedback`](core/feedback/) · [`statistics`](core/statistics/) · [`tracing`](core/tracing/)

**Tiers (`nitro!`)**: [`dual_mode`](core/dual_mode/)

**Concurrency**: [`threading`](core/threading/) · [`spawn`](core/spawn/) · [`async`](core/async/) · [`async_source`](core/async_source/)

**Dynamism** ([full index](core/dynamism/)): [`dynamic_group`](core/dynamism/dynamic_group/) · [`dynamic_manual`](core/dynamism/dynamic_manual/) · [`demux_it`](core/dynamism/demux_it/) · [`demux_map`](core/dynamism/demux_map/)

### Adapters — [full index](adapters/)

**No server needed**: [`lines`](adapters/lines/) · [`csv`](adapters/csv/) · [`augurs`](adapters/augurs/) · [`zmq`](adapters/zmq/)

**Brokers**: [`kafka`](adapters/kafka/) · [`fluvio`](adapters/fluvio/) · [`redis`](adapters/redis/) · [`etcd`](adapters/etcd/)

**Low latency**: [`iceoryx2`](adapters/iceoryx2/) · [`aeron`](adapters/aeron/)

**Stores**: [`kdb`](adapters/kdb/) · [`postgres`](adapters/postgres/)

**Protocols / web**: [`fix`](adapters/fix/) · [`web`](adapters/web/)

**Market data**: [`market`](adapters/market/)

**Telemetry**: [`prometheus`](adapters/prometheus/) · [`otlp`](adapters/otlp/) · [`telemetry`](adapters/telemetry/)

### Showcase — [full index](showcase/)

[`latency`](showcase/latency/) — per-hop stamping over iceoryx2 ·
[`trading_e2e`](showcase/trading_e2e/) — nine stages, browser to venue and back

## Target names vs directory names

Directories are named after the *thing* (`adapters/csv/`); example targets keep
their historical names (`csv_adapter`), so `cargo run --example csv_adapter` works
exactly as it always has. Each group's index table lists both, and every target is
declared explicitly in [`../Cargo.toml`](../Cargo.toml) under `# Examples`.

## Adding an example

1. Create `examples/<group>/<name>/` with `main.rs` **and** `README.md`.
2. Add an `[[example]]` block to [`../Cargo.toml`](../Cargo.toml) with an explicit
   `path` (`autoexamples` is off).
3. Add a row to the group's `README.md` and, if it earns a place, to this file.

`scripts/check-example-docs.sh` enforces steps 1 and 3 in CI.

**Several examples of one thing?** Nest them: `examples/<group>/<topic>/<name>/`,
each `<name>/` still carrying its own `main.rs` + `README.md`, plus a `README.md`
at `<topic>/` indexing them and any code they share. That is how
[`core/dynamism/`](core/dynamism/) (four wirings of one price book over a shared
`market_data.rs`) and [`adapters/kdb/`](adapters/kdb/) are laid out. Target
*names* stay flat and must not change when a directory moves — `core/dynamism/
demux_it/` holds the target `demux`, so `cargo run --example demux` keeps
working.

## Elsewhere

- [`../../README.md`](../../../README.md) — Wingfoil overview and quick start
- [`../benches/`](../benches/) — benchmarks, including the three-tier comparison
- [`../src/adapters/`](../src/adapters/) — the adapter implementations
- [`../../../docs/planning/port-plan.md`](../../../docs/planning/port-plan.md) — the port roadmap and capability matrix

//! The dual-mode thesis, completed by the `nitro!` macro: **one** wiring
//! definition expands to both an interpreted runner and a fully
//! monomorphized compiled runner. The two cannot drift — same tokens, same
//! `Op` semantics — and the compiled one gets the compiler's full
//! optimization across node boundaries.
//!
//! The wiring is a **split/recombine** DAG: a counter is split on parity into
//! two labelled branches which are then merged back into one stream — so this
//! also shows the macro deriving a non-linear graph (a shared apex node and a
//! recombine) for both engines:
//!
//! ```text
//!               count                 (apex — shared node, once/cycle)
//!              /     \
//!       is_odd?      is_even?         (split on parity)
//!            |          |
//!      "{i} is odd" "{i} is even"     (format each branch)
//!              \     /
//!               merge                 (recombine — at most one fires/tick)
//!                 |
//!               log tap               (emit as the run progresses)
//! ```
//!
//! The tail emits as the run progresses — a closure sink over the `log` crate,
//! per the house rule that examples stream their output rather than collect it.
//! Two things worth knowing about that tap:
//!
//! - It is spelled `with_time().for_each(|(t, s)| { log::info!(..); Ok(()) })`
//!   rather than `.logged(..)`, because `logged` is deliberately
//!   **fluent-only**: its op `Cfg` is `(String, log::Level)` while the fluent
//!   method takes `&str`, and `nitro!` uses the call-site argument types as
//!   the `Cfg` verbatim — the same tokens cannot satisfy both (see
//!   `tests/op_completeness.rs`, category 2b). A closure sink is the same tap
//!   and works identically on every tier, because closures are opaque config.
//! - This example **runs** the graph; it does not tie the engines out against
//!   each other. That assertion needs both runs' whole output held as a value,
//!   which is `accumulate()`'s job and a test's place —
//!   [`tests/macro_parity.rs`](../../../tests/macro_parity.rs) pins this same
//!   diamond's interpreted and compiled outputs equal (with an `accumulate`
//!   tail in place of the log tap, which is what lets it hold the output).
//!
//! `log` output is rendered by `env_logger`, so run with `RUST_LOG=info`:
//!
//! ```sh
//! RUST_LOG=info cargo run -p wingfoil --release --example dual_mode
//! ```
//!
//! # What procedural code you can write inside `nitro!`
//!
//! The macro parses its body as a plain Rust `fn`, but it does not *run* that
//! code — it reads the tokens to derive a **static DAG** at expansion time,
//! then re-emits the whole schedule three ways (`interpreted` / `compiled` /
//! `nested`). Because `compiled()` monomorphizes one local per node (see the
//! committed expansion in [`expanded/`](expanded/)), the node list must be
//! complete and fixed after parsing. That is the one rule everything below
//! follows: **wiring must be straight-line — the shape of the graph cannot
//! depend on runtime values.** Values and per-element logic can be as
//! procedural as you like; the *topology* cannot.
//!
//! Each top-level statement is sorted into one of three buckets: a **wiring**
//! `let name = <chain>;` (rooted at the builder or an already-bound stream),
//! the **tail** expression naming the outputs, or **passthrough** (anything
//! else, re-emitted verbatim into every engine). The builder and stream names
//! may appear *only* in wiring statements and the tail.
//!
//! ## ✅ Allowed
//!
//! ```rust,ignore
//! // Straight-line wiring: each `let` is one fluent chain.
//! let count  = g.ticker(PERIOD).count();
//! let parity = count.map(|i| i % 2);
//!
//! // Passthrough: ordinary Rust that does NOT mention `g` or a stream —
//! // compute a config, declare a local a closure will capture, etc.
//! let base = 2;
//! let threshold = base * 4;
//! let tagged = count.filter(&parity).map(move |i| i + threshold);
//!
//! // Any control flow you want *inside* an op closure — it is opaque config
//! // the macro passes straight through, never topology.
//! let label = count.map(|i| if i % 2 == 0 { "even" } else { "odd" });
//!
//! // Static repetition sugar with a LITERAL count: `map_n` chains N maps,
//! // `fan` builds N branches and merges them. The DAG stays static.
//! let chained = count.map_n(3, |i| i + 1);
//! let fanned  = count.fan(2, |s| s.map(|i| i * 10));
//! ```
//!
//! ## ❌ Not allowed
//!
//! ```rust,ignore
//! // A helper that does wiring — the macro cannot see the nodes it builds,
//! // so `compiled()` would be blind to them. (Compose by NESTING nitro!s
//! // instead: each nitro! fn is itself reusable wiring via its `wire`.)
//! let x = build_subgraph(g, &count);
//!
//! // A loop that wires — the node count would be a runtime value, which
//! // `compiled()` cannot monomorphize. Use `.map_n`/`.fan` with a literal.
//! for _ in 0..n { count = count.map(|i| i + 1); }
//!
//! // A conditional that picks the TOPOLOGY — one branch or the other would
//! // exist depending on a runtime flag. (Branch *inside* a closure instead,
//! // or build both and select at runtime.)
//! let s = if fast { count.ema(2) } else { count.ema(8) };
//!
//! // A non-literal repeat count — the unrolled DAG must be known statically.
//! let chained = count.map_n(n, |i| i + 1);
//! ```
//!
//! The full expansion of the `nitro!` block below — `wire`, `interpreted`,
//! `compiled`, `run`, and `nested` — is committed **verbatim** in
//! [`expanded/main.expanded.rs`](expanded/main.expanded.rs), so you can read
//! exactly what "straight-line wiring becomes a static schedule" means in
//! emitted code. `expanded/README.md` has the command that regenerates it.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode, Tier};

const HISTORICAL: RunMode = RunMode::HistoricalFrom(NanoTime::ZERO);
const CYCLES: u32 = 10;
const PERIOD: Duration = Duration::from_millis(10);

// One definition — a valid fluent wiring function whose DAG is a
// split/recombine. The macro parses it to derive the DAG and expands to a
// module: `odds_evens::wire` (this function, verbatim),
// `odds_evens::interpreted()` (built through wire), `odds_evens::compiled()`
// (the monomorphized schedule derived from the same tokens), and
// `odds_evens::run(tier, ..)` (either engine behind one signature).
//
// `count` is referenced three times, so it is a *shared* apex node: the
// interpreted engine runs it once per cycle and fans the tick out, and the
// compiled engine emits it once and feeds every reader from the same slot.
// `merge` is the recombine — since a number is either odd or even, at most
// one branch fires on any tick. `sink` is a side-effect-only node (nothing
// reads it, it is not in the tail): the log tap that streams each label out
// through the `log` crate, stamped with the engine time it ticked at.
wingfoil::nitro! {
    fn odds_evens(g: &GraphBuilder) -> Stream<String> {
        let count = g.ticker(PERIOD).count();
        let is_even = count.map(|i| i.is_multiple_of(2));
        let is_odd = is_even.map(|b| !b);
        let odd_str = count.filter(&is_odd).map(|i| format!("{i} is odd"));
        let even_str = count.filter(&is_even).map(|i| format!("{i} is even"));
        let labelled = odd_str.merge(&even_str);
        let sink = labelled.with_time().for_each(|(t, s): &(NanoTime, String)| {
            log::info!(target: "wingfoil", "{} odds/evens {s:?}", t.pretty());
            Ok(())
        });
        labelled
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();

    // `Tier::default()` resolves from `WINGFOIL_TIER` if it is set and
    // otherwise from the build profile (interpreted in debug, compiled in
    // release), so the usual workflow — develop interpreted, deploy compiled —
    // needs no call-site change at all. The log lines are identical either
    // way; that the tiers *agree* is pinned by `tests/macro_parity.rs`.
    let tier = Tier::default();
    let (last,) = odds_evens::run(tier, HISTORICAL, RunFor::Cycles(CYCLES))?;

    println!("ran {CYCLES} cycles on the {tier} tier; last label: {last:?}");
    Ok(())
}

//! async / tokio integration: an async producer of *timestamped* values driving
//! a wingfoil graph — the legacy `produce_async` model, ported to wingfoil and
//! run in **both** modes off one definition.
//!
//! Async streams are a natural fit for IO but an awkward one for business
//! logic: their execution is implicit and path-at-a-time. Wingfoil's is
//! explicit, topologically sorted and time-aware (historical *and* realtime). The
//! [`produce_async`] bridge keeps the best of both: IO lives in the async
//! producer, business logic lives in the graph, and the boundary between them
//! is a single typed edge.
//!
//! [`produce_async`] maps an async [`futures::Stream`] of `(NanoTime, T)` onto
//! a graph source; the graph itself is the consumer (legacy hands the stream
//! to an async `consume_async` closure — on wingfoil, an on-graph `for_each` plays
//! that role, keeping the consumer in the explicitly-timed world). The producer
//! runs on the graph's own tokio runtime (created lazily) and each value wakes
//! the kernel.
//!
//! Because each value carries its **own** event time, the same producer serves
//! a live feed and a recorded one. The closure is handed the run's
//! [`RunParams`], so it stamps arrivals off the wall clock in realtime and
//! replays its recorded event times in a historical run — same wiring, same
//! values, deterministic replay. That is the difference from
//! [`async_source`](../async_source/), whose `external` source is driven by
//! wall-clock arrivals and so only makes sense live.
//!
//! Gated behind the `async` feature (tokio + futures):
//!
//! ```sh
//! cargo run --manifest-path crates/wingfoil/Cargo.toml --features async --example async
//! ```

use std::time::Duration;

use wingfoil::async_source::{RunParams, produce_async};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// Quotes in the feed. It is finite: the producer returns `None` at the end,
/// closing the stream, which is what stops a `RunFor::Forever` run.
const N: u32 = 8;

/// How long the producer awaits between yields — a stand-in for a socket read.
/// Wall-clock time, so it paces the realtime run and merely delays (but does
/// not shape) the historical one.
const IO_DELAY: Duration = Duration::from_millis(1);

/// Spacing of the recorded feed's **event** timestamps. In a historical run
/// these are the graph times the values replay at, however fast the producer
/// happens to yield them.
const PERIOD: Duration = Duration::from_millis(10);

/// Run the graph in `run_mode`, returning every quote it saw and the mean.
fn run(run_mode: RunMode) -> anyhow::Result<(Vec<f64>, f64)> {
    // The graph owns the tokio runtime (created lazily); no `&Handle` to pass.
    let g = GraphBuilder::new();

    // The async producer: it awaits between yields (as a socket read would) and
    // emits timestamped values. One definition, both modes — `params` carries
    // the run the graph actually started, so the producer knows which clock its
    // timestamps should come from.
    let quotes = produce_async(
        &g,
        move |params: RunParams| async move {
            let historical = matches!(params.run_mode, RunMode::HistoricalFrom(_));
            Ok(futures::stream::unfold(
                (0u32, 100.0_f64),
                move |(i, price)| async move {
                    if i >= N {
                        return None; // the feed ends, closing the stream
                    }
                    tokio::time::sleep(IO_DELAY).await; // simulate waiting IO
                    let price = price + (i as f64 % 3.0) - 1.0;
                    // Historical: the event's own recorded time, which the
                    // engine replays at. Realtime: the value ticks on arrival,
                    // so the stamp is the arrival instant.
                    let time = if historical {
                        params.start_time + PERIOD * i
                    } else {
                        NanoTime::now()
                    };
                    Some((Ok((time, price)), (i + 1, price)))
                },
            ))
        },
        None,
    )?;

    // The consumer, on the graph: print each arriving quote at its graph time,
    // relative to the first — realtime graph times are absolute wall-clock
    // instants, so an offset is what reads. `for_each_mut` owns that baseline
    // (`for_each` takes an `Fn`, so it has nowhere to put it). A burst carries
    // everything that landed at the same instant, so iterate it rather than
    // taking the last value.
    let _printed = quotes.with_time().for_each_mut(
        None::<NanoTime>,
        |base: &mut Option<NanoTime>, (time, burst): &(NanoTime, Burst<f64>)| {
            let t0 = *base.get_or_insert(*time);
            for price in burst.iter() {
                println!("  +{:>5.1} ms  {price:>6.2}", f64::from(*time - t0) / 1e6);
            }
            Ok(())
        },
    );

    // `collapse_accumulate` flattens the bursts and accumulates in one step —
    // the burst-aware counterpart to `accumulate()`, so nothing is lost when a
    // realtime cycle carries several quotes.
    let seen = quotes.collapse_accumulate();
    let mean = seen.map(|qs| qs.iter().sum::<f64>() / qs.len().max(1) as f64);

    let mut runner = g.build();
    // `Forever` is bounded by the feed itself: the stream closes after N values.
    runner.run(run_mode, RunFor::Forever)?;
    Ok((runner.value(&seen), runner.value(&mean)))
}

fn main() -> anyhow::Result<()> {
    // Historical: the producer's recorded timestamps are the graph clock, so
    // the quotes land 10 ms apart in graph time however fast they are produced.
    println!("historical replay (event times from the feed):");
    let (historical, historical_mean) = run(RunMode::HistoricalFrom(NanoTime::ZERO))?;
    println!("  mean {historical_mean:.3}");

    // Realtime: the same producer, values ticking as they arrive.
    println!("\nrealtime (graph times are arrival times):");
    let (realtime, realtime_mean) = run(RunMode::RealTime)?;
    println!("  mean {realtime_mean:.3}");

    // Same feed, same values — only the tick times differ. That is what makes
    // a recorded feed replayable: back-test the wiring, then deploy it.
    assert_eq!(historical, realtime, "both modes must see the same quotes");
    Ok(())
}

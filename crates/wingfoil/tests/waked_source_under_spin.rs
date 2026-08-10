//! **A wake-driven source keeps delivering while the kernel busy-spins.**
//!
//! The invariant behind "develop against the waker, deploy to busy-spin": those
//! are not two sources, they are one source under two kernel *waiting*
//! strategies. `Kernel::begin_cycle` calls `drain_ready` **before** it branches
//! on `spin` (`runtime/kernel.rs`), so a `KernelWaker` firing lands in `dirty`
//! either way — `spin` only decides whether the kernel parks between cycles.
//!
//! # Why this needs pinning
//!
//! Without it the two ingest styles are structurally different graphs, and that
//! is a parity hole with teeth. `poll` (`Activation::ALWAYS`) is **realtime
//! only** — it rejects `HistoricalFrom` outright, since there is nothing to
//! busy-poll in a deterministic replay — while `channel`
//! (`Activation::THREADED`) replays timestamped sends deterministically. Since
//! the house convention is that tests run historically and assert exact values
//! *and* tick times, a deployment built on `poll` could never be covered by a
//! test built on `channel`: you would verify one graph and ship another.
//!
//! This file says that trade is unnecessary. One `channel` wiring serves the
//! deterministic test, the parked realtime run, and the spinning realtime run.
//!
//! # What this does *not* yet establish
//!
//! Only the interpreted engine. `compiled()` still constructs its `Kernel`
//! without a ready receiver, so a `THREADED` node's dirty bit can never be set
//! there — see the `nitro!` module docs. Closing that needs the receiver wired
//! into the generated runner *and* an entry point that lets the producer handle
//! escape before the run begins (`compiled()` returns only once it is over).
//!
//! **Realtime, so timing-dependent by nature.** The budget below is ~8x the
//! producer's total sleep, which is the margin that keeps it honest without
//! making it flaky.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{Burst, RunFor, RunMode};

/// Five sends, 5ms apart, against a 200ms run.
const SENDS: u64 = 5;
const GAP: Duration = Duration::from_millis(5);
const BUDGET: Duration = Duration::from_millis(200);

/// Wire a `channel` source into an accumulating sink, spawn a producer, run,
/// and return what arrived. `busy` adds a `poll` node — the only public way to
/// put the kernel into spin mode, since `Runner::run` enables it from
/// `has_always`.
fn run_with_channel(busy: bool) -> Vec<u64> {
    let g = GraphBuilder::new();

    if busy {
        // Its values are irrelevant: this node exists purely to flip `spin`.
        let polls = AtomicU64::new(0);
        let _ = g.poll(move || {
            polls.fetch_add(1, Ordering::Relaxed);
            None::<u64>
        });
    }

    let (values, sender) = g.channel::<u64>();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    values.for_each(move |b: &Burst<u64>| {
        sink.lock()
            .expect("sink mutex poisoned")
            .extend(b.iter().copied());
        Ok(())
    });

    let producer = std::thread::spawn(move || {
        for i in 1..=SENDS {
            std::thread::sleep(GAP);
            assert!(sender.send(i), "the graph went away mid-send");
        }
    });

    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Duration(BUDGET))
        .expect("run");
    producer.join().expect("producer panicked");

    seen.lock().expect("sink mutex poisoned").clone()
}

/// **The property**: spinning does not cost the wake path. Every value the
/// producer sent arrives, in order, exactly as when the kernel parks.
#[test]
fn a_waked_source_delivers_while_the_kernel_spins() {
    assert_eq!(vec![1, 2, 3, 4, 5], run_with_channel(true));
}

/// The control, so the assertion above is a comparison rather than a hope: the
/// same wiring with no `ALWAYS` node, so the kernel parks between cycles.
#[test]
fn the_same_source_delivers_when_the_kernel_parks() {
    assert_eq!(vec![1, 2, 3, 4, 5], run_with_channel(false));
}

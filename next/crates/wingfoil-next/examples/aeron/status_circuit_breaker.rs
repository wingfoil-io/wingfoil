//! Circuit-breaker example driven by the Aeron status side-channel.
//!
//! A port of the classic `wingfoil/examples/aeron/status_circuit_breaker.rs`
//! onto the next engine. [`aeron_sub_fragment_with_status`] returns
//! `(data, status)`; here the status half drives a "healthy" gate — while the
//! subscriber's last observed [`AeronStatus`] is `Connected`, incoming values
//! pass through; otherwise they are dropped (and the drop is logged).
//!
//! Where classic needed a hand-written `MutableNode` reading both streams, next
//! expresses the same thing with the ordinary fluent vocabulary: `fold` latches
//! the latest transition into a running status, and a two-input `custom_node`
//! gates the data on it (the status is a *passive* upstream, so the gate fires
//! on data, not on status).
//!
//! Run with:
//! ```sh
//! cargo run -p wingfoil-next --example aeron_status_circuit_breaker --features aeron
//! ```

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use wingfoil_next::adapters::aeron::{
    AeronHandle, AeronSinkOps, AeronStatus, AeronSubOptions, FragmentBuffer, TransportError,
    aeron_sub_fragment_with_status,
};
use wingfoil_next::op::{Activation, Tick};
use wingfoil_next::prelude::*;
use wingfoil_next::{RunFor, RunMode};

fn main() -> anyhow::Result<()> {
    env_logger::init();

    let handle = AeronHandle::connect()?;
    let channel = "aeron:ipc";
    let stream_id = 1010i32;
    let timeout = Duration::from_secs(5);

    let g = GraphBuilder::new();

    let (data, status) = aeron_sub_fragment_with_status(
        &g,
        RunMode::RealTime,
        handle.subscription(channel, stream_id, timeout)?,
        |f: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
            Ok(f.as_ref().try_into().ok().map(i64::from_le_bytes))
        },
        AeronSubOptions::default(),
    )?;

    // Feed the stream so there is something to gate.
    let _publisher = g
        .ticker(Duration::from_millis(50))
        .count()
        .map(|n: &u64| burst![*n as i64])
        .aeron_pub(
            handle.publication(channel, stream_id, timeout)?,
            |v: &i64| v.to_le_bytes().to_vec(),
        );

    // Latch the latest transition into a running status, logging each change.
    let latched = status.fold(
        AeronStatus::default(),
        |last, burst: &Burst<AeronStatus>| {
            if let Some(latest) = burst.last()
                && *latest != *last
            {
                println!("breaker: status {last:?} → {latest:?}");
                *last = *latest;
            }
        },
    );

    // Gate the data on the latched status: `latched` is a *passive* upstream, so
    // the gate fires when data arrives, reading whatever status is current.
    let counters = Rc::new(RefCell::new((0u64, 0u64)));
    let cycle_counters = counters.clone();
    let data_slot = data.value_slot();
    let status_slot = latched.value_slot();
    let _gate = g.custom_node::<Burst<i64>, _>(
        &[data.upstream()],
        &[latched.upstream()],
        Activation::NONE,
        move |_ctx| {
            let healthy = matches!(*status_slot.borrow(), AeronStatus::Connected);
            let burst: Burst<i64> = data_slot.borrow().clone();
            let (passed, dropped) = &mut *cycle_counters.borrow_mut();
            if healthy {
                *passed += burst.len() as u64;
                Ok(Tick::Value(burst))
            } else {
                *dropped += burst.len() as u64;
                println!("breaker OPEN: dropped {} value(s)", burst.len());
                Ok(Tick::Quiet)
            }
        },
    );

    g.build()
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(3)))?;

    let (passed, dropped) = *counters.borrow();
    println!("breaker: passed {passed}, dropped {dropped}");
    Ok(())
}

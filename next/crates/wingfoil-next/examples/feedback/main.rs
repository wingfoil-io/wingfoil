#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run -p wingfoil-next --example feedback
//! ```

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::prelude::*;

fn main() -> anyhow::Result<()> {
    let period = Duration::from_secs(1);
    let setpoint = 20.0_f64; // target temperature
    let gain = 0.5_f64; // proportional controller gain

    let g = GraphBuilder::new();

    // A clock to advance the control loop one step per period.
    let clock = g.ticker(period).count();

    // The feedback edge carries the room temperature. `temp_rx` lets us
    // reference the temperature *before* the plant that produces it exists —
    // which is what makes the loop below constructible at all. Values sent to
    // `temp_tx` arrive on `temp_rx` one cycle later, so the graph stays acyclic.
    let (temp_rx, temp_tx) = g.feedback::<f64>();

    // Controller: how far are we from the setpoint? `join_passive` reads the
    // fed-back temperature *passively* — the clock drives the tick, the
    // temperature is merely read — so `error` advances once per period.
    let error = clock.join_passive(&temp_rx, move |_, temp| setpoint - temp);

    // Control signal: how hard to drive the heater.
    let heater = error.map(move |e| gain * e);

    // Plant: the room. New temperature = old temperature + heater output.
    // Again the old temperature is read passively; the heater drives the tick.
    let plant = heater.join_passive(&temp_rx, |power, temp| temp + power);

    // Close the loop: feed each new temperature back so the next step's `error`
    // can read it. plant -> heater -> error -> plant is a genuine cycle; the
    // feedback edge is the only construct that can express it.
    let temperature = plant.feedback(&temp_tx);

    let _out = temperature.for_each(|t| {
        println!("temperature: {t:.3}");
        Ok(())
    });

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Duration(period * 7),
    )?;

    Ok(())
}

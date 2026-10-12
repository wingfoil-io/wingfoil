#![doc = include_str!("./README.md")]
//!
//! ```sh
//! cargo run -p wingfoil --example feedback
//! ```

use std::time::Duration;

use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

fn main() -> anyhow::Result<()> {
    let period = Duration::from_secs(1);
    let setpoint = 20.0_f64; // target temperature
    let gain = 0.5_f64; // proportional controller gain

    let g = GraphBuilder::new();

    // A clock to advance the control loop one step per period.
    let clock = g.ticker(period).count();

    // The loop. `feedback_loop` hands the body the room temperature from the
    // step before — `temp_was` — and sends round the new temperature the body
    // returns, to arrive one cycle later. That is what lets the controller
    // read a temperature the plant has not produced yet, and it keeps the
    // graph acyclic: plant -> heater -> error -> plant is a genuine cycle, cut
    // in time.
    let temperature = g.feedback_loop(|temp_was| {
        // Controller: how far are we from the setpoint? `join_passive` reads
        // the temperature *passively* — the clock drives the tick, the
        // temperature is merely read — so `error` advances once per period.
        let error = clock.join_passive(temp_was, move |_, temp: &f64| setpoint - temp);

        // Control signal: how hard to drive the heater.
        let heater = error.map(move |e| gain * e);

        // Plant: the room. New temperature = old temperature + heater output.
        // Again the old temperature is read passively; the heater drives the
        // tick. This is what goes round the loop.
        heater.join_passive(temp_was, |power, temp: &f64| temp + power)
    });

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

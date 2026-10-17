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
    // names as `feedback`, to arrive one cycle later. That is what lets the controller
    // read a temperature the plant has not produced yet, and it keeps the
    // graph acyclic: plant -> heater -> error -> plant is a genuine cycle, cut
    // in time.
    let (temperature, heater) = g.feedback_loop(|temp_was| {
        // Controller: how far are we from the setpoint? `join_passive` reads
        // the temperature *passively* — the clock drives the tick, the
        // temperature is merely read — so `error` advances once per period.
        let error = clock.join_passive(temp_was, move |_, temp: &f64| setpoint - temp);

        // Control signal: how hard to drive the heater.
        let heater = error.map(move |e| gain * e);

        // Plant: the room. New temperature = old temperature + heater output.
        // Again the old temperature is read passively; the heater drives the
        // tick. This is what goes round the loop.
        let temperature = heater.join_passive(temp_was, |power, temp: &f64| temp + power);

        // `feedback` goes round the loop; `out` is handed back beside it
        // without going round — here the heater, so it can be printed too.
        // Returning `temperature` alone would do when nothing else is wanted.
        FeedbackLoop {
            feedback: temperature,
            out: heater,
        }
    });

    let _out = temperature
        .join_passive(&heater, |t, power: &f64| (*t, *power))
        .for_each(|(t, power)| {
            println!("heater: {power:.3}  temperature: {t:.3}");
            Ok(())
        });

    let mut runner = g.build();
    runner.run(
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Duration(period * 7),
    )?;

    Ok(())
}

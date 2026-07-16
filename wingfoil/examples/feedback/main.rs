#![doc = include_str!("./README.md")]

use std::time::Duration;

use wingfoil::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let period = Duration::from_secs(1);
    let setpoint = 20.0_f64; // target temperature
    let gain = 0.5_f64; // proportional controller gain

    // A clock to advance the control loop one step per period.
    let clock = ticker(period).count();

    // The feedback channel carries the room temperature. `temp_rx` lets us
    // reference the temperature *before* the plant that produces it exists —
    // which is what makes the loop below constructible at all.
    let (temp_tx, temp_rx) = feedback::<f64>();

    // Controller: how far are we from the setpoint, and how hard to drive.
    // `error` reads the fed-back temperature (passive: the clock drives ticks).
    let error = bimap(
        Dep::Active(clock),
        Dep::Passive(temp_rx.clone()),
        move |_, temp| setpoint - temp,
    );
    // Control signal: how hard to drive the heater.
    let heater = error.map(move |e| gain * e);

    // Plant: the room. New temperature = old temperature + heater output.
    let plant = bimap(Dep::Active(heater), Dep::Passive(temp_rx), |power, temp| {
        temp + power
    });

    // Close the loop: feed each new temperature back so the next step's
    // `error` can read it. plant -> heater -> error -> plant is a genuine
    // cycle; the feedback channel is the only edge that can express it.
    let temperature = plant.feedback(temp_tx);

    temperature
        .for_each(|t, _| println!("temperature: {t:.3}"))
        .run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Duration(period * 7),
        )?;

    Ok(())
}

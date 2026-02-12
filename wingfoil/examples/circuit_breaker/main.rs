#![doc = include_str!("./README.md")]

use log::Level::Info;
use wingfoil::*;

use std::time::Duration;

fn main() {
    env_logger::init();
    let period = Duration::from_micros(100);
    let lookback = 5;
    let level: i64 = 3;

    // Source: a counter ticking every 100us
    let source = ticker(period).count();

    // Create a feedback channel carrying () — just a tick signal.
    let (tx, rx) = feedback_node();

    // delay_with_reset emits the source value delayed by `lookback` periods.
    // When the trigger (rx) fires, the delay resets: the output snaps to the
    // current source value and the pending queue is cleared.
    let delayed = source.delay_with_reset(period * lookback, rx);

    // The delayed stream is a Passive dependency — it is read when diff ticks
    // but does not itself trigger diff. Only the source (Active) triggers it.
    // This matters because after a reset, delayed ticks with the snapped value;
    // if it were Active it would cause an extra diff evaluation at that moment.
    let diff = bimap(Dep::Active(source), Dep::Passive(delayed), |a, b| {
        a as i64 - b as i64
    })
    .logged("diff", Info);

    // When |diff| exceeds the level, fire the feedback to reset the delay
    let trigger = diff
        .filter_value(move |p| p.abs() > level)
        .logged("trigger", Info)
        .feedback_node(tx);

    Graph::new(
        vec![trigger],
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Duration(period * 14),
    )
    .run()
    .unwrap();
}

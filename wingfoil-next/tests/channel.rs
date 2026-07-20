//! Phase 3 channel layer: cross-thread value transport with the [`Message`]
//! envelope, ported onto the Op model. Mirrors the classic channel's
//! observable behaviour — values arrive in order, a producer error
//! propagates into the graph (aborting the run, via the Phase 0.1 fallible
//! cycle), and the message envelope's equality matches the classic one.

use std::time::Duration;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::channel::Message;
use wingfoil_next::fluent::GraphBuilder;

/// A producer thread sends values through the channel; the graph receives
/// them (in order, the last one landing in the accumulator). The channel
/// wakes the realtime kernel on each send — the external-source pattern with
/// the richer envelope.
#[test]
fn channel_delivers_values_across_threads() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let acc = values.accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        for i in 1..=5 {
            sender.send(i);
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    r.run(RunMode::RealTime, RunFor::Cycles(5)).unwrap();
    producer.join().expect("producer thread");

    let got = r.value(&acc);
    assert!(!got.is_empty(), "at least one value must arrive");
    assert!(got.windows(2).all(|w| w[0] < w[1]), "in order: {got:?}");
    assert_eq!(Some(&5), got.last(), "final value must arrive: {got:?}");
}

/// A producer that sends `Message::Error` aborts the receiving graph's run,
/// with the error surfaced through the channel node's context — the channel
/// layer leaning on Phase 0.1's fallible cycle.
#[test]
fn channel_error_aborts_the_run() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let _acc = values.accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send(1);
        std::thread::sleep(Duration::from_millis(2));
        sender.send_error(anyhow::anyhow!("producer blew up"));
    });
    let result = r.run(RunMode::RealTime, RunFor::Cycles(20));
    producer.join().expect("producer thread");

    let err = result.expect_err("a producer error must abort the run");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("channel"),
        "error names the channel node: {msg}"
    );
    assert!(
        msg.contains("producer blew up"),
        "error chains cause: {msg}"
    );
}

/// `close()` (end-of-stream) is a clean no-value signal — the run terminates
/// normally once the producer is done, carrying the values seen so far.
#[test]
fn channel_close_is_clean() {
    let g = GraphBuilder::new();
    let (values, sender) = g.channel::<u64>();
    let acc = values.accumulate();
    let mut r = g.build();

    let producer = std::thread::spawn(move || {
        sender.send(42);
        std::thread::sleep(Duration::from_millis(2));
        sender.close();
    });
    // Bounded so the test can't hang if the close signal is missed.
    r.run(RunMode::RealTime, RunFor::Cycles(3)).unwrap();
    producer.join().expect("producer thread");
    assert_eq!(Some(&42), r.value(&acc).last());
}

/// The message envelope's equality mirrors the classic `Message`: values and
/// checkpoints compare structurally, end-of-stream is a unit, and errors
/// never compare equal.
#[test]
fn message_equality_matches_classic() {
    assert_eq!(Message::<u64>::Value(7), Message::Value(7));
    assert_ne!(Message::<u64>::Value(7), Message::Value(8));
    let t = NanoTime::new(42);
    assert_eq!(Message::<u64>::Checkpoint(t), Message::Checkpoint(t));
    assert_eq!(Message::<u64>::EndOfStream, Message::EndOfStream);
    // Errors never compare equal.
    let e1 = Message::<u64>::Error(std::sync::Arc::new(anyhow::anyhow!("x")));
    let e2 = Message::<u64>::Error(std::sync::Arc::new(anyhow::anyhow!("x")));
    assert_ne!(e1, e2);
}

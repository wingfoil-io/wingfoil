use async_stream::stream;
use futures::StreamExt;
use std::boxed::Box;
use std::pin::Pin;
use std::time::Duration;
use wingfoil::*;

fn main() {
    env_logger::init();
    let period = Duration::from_millis(10);
    let run_for = RunFor::Duration(period * 5);
    let run_mode = RunMode::RealTime;

    let producer = async move || {
        stream! {
            for i in 0.. {
                tokio::time::sleep(period).await; // simulate waiting IO
                let time = NanoTime::now();
                yield (time, i * 10);
            }
        }
    };

    let consumer = async move |mut source: Pin<Box<dyn FutStream<u32>>>| {
        while let Some((time, value)) = source.next().await {
            println!("{time:?}, {value:?}");
        }
    };

    produce_async(producer)
        .logged("on-graph", log::Level::Info)
        .collapse()
        .consume_async(Box::new(consumer))
        .run(run_mode, run_for)
        .unwrap();
}

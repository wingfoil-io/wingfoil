## async / tokio integration

In this example, we demonstrate wingfoil's integration with tokio / async.
This makes building IO adapters at the the graph edge a breeze, giving 
the best of sync and async worlds.   

Async streams are a useful abstraction for IO but are not as powerful or 
as easy to use for implementing complex business logic as wingfoil.  Async 
uses an implicit, depth first graph execution strategy.  In
contrast wingfoil's explicit, breadth first graph execution algorithm, 
is easier to reason about, encourages re-use and provides a more 
structured and productive development environment.   Wingfoils 
explicit modelling of time with support for historical and realtime 
modes, is also huge win in terms of productivity and ability to 
backtest strategies over async streams.


The key methods here are produce_async which maps an async futures stream 
into a wingfoil stream and consume_async which takes a wingfoil stream
and consumes it with a supplied async closure.

This hybrid sync-async approach enforces clear seperation 
between IO and business logic, which can often be problematic in 
async oriented systems.   




```rust
use async_stream::stream;
use std::time::Duration;
use std::pin::Pin;
use futures::StreamExt;
use wingfoil::*;

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
    .run(run_mode, run_for);
```


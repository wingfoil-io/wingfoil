# Threading Example

This example demonstrates multi-threaded graph execution using `producer()` and `mapper()`.

- `producer()`: Runs a sub-graph on a worker thread, sending values to the main graph
- `mapper()`: Receives values, processes them on a worker thread, sends results back

The sub-graphs are wired and executed on their own dedicated threads, using channels to
send data between them.

Historical and RealTime modes are supported. In RealTime mode, data can arrive in bursts
i.e. multiple inputs may have been received since the last engine cycle, so the incoming
data is always a vector of the source data. In this example, we use the collapse method
to collapse the burst into a single (latest) value.

## Code

```rust
use log::Level::Info;
use std::rc::Rc;
use std::thread;
use std::time::Duration;
use tinyvec::TinyVec;
use wingfoil::*;

fn label(name: &str) -> String {
    format!("{:?} >> {:9}", thread::current().id(), name)
}

fn main() {
    env_logger::init();
    let period = Duration::from_millis(100);
    let run_mode = RunMode::RealTime;
    let run_for = RunFor::Duration(period * 6);

    let produce_graph = move || {
        let label = label("producer");
        ticker(period).count().logged(&label, Info)
    };

    let map_graph = |src: Rc<dyn Stream<TinyVec<[u64; 1]>>>| {
        let label = label("mapper");
        src.collapse()
            .map(|x| x * 10)
            .logged(&label, Info)
    };

    producer(produce_graph)
        .collapse()
        .logged(&label("main-pre"), Info)
        .mapper(map_graph)
        .collapse()
        .logged(&label("main-post"), Info)
        .run(run_mode, run_for)
        .unwrap();
}
```

## Running

```
RUST_LOG=info cargo run --example threading
```

## RealTime Mode Output

```
[2026-01-18T11:55:05Z INFO  wingfoil] 0.000_022 ThreadId(6) >> producer  1
[2026-01-18T11:55:05Z INFO  wingfoil] 0.000_006 ThreadId(1) >> main-pre  1
[2026-01-18T11:55:05Z INFO  wingfoil] 0.000_127 ThreadId(7) >> mapper    10
[2026-01-18T11:55:05Z INFO  wingfoil] 0.000_247 ThreadId(1) >> main-post 10
[2026-01-18T11:55:05Z INFO  wingfoil] 0.100_159 ThreadId(6) >> producer  2
[2026-01-18T11:55:05Z INFO  wingfoil] 0.101_803 ThreadId(1) >> main-pre  2
[2026-01-18T11:55:05Z INFO  wingfoil] 0.101_956 ThreadId(7) >> mapper    20
[2026-01-18T11:55:05Z INFO  wingfoil] 0.101_803 ThreadId(1) >> main-post 20
[2026-01-18T11:55:06Z INFO  wingfoil] 0.200_295 ThreadId(6) >> producer  3
[2026-01-18T11:55:06Z INFO  wingfoil] 0.200_521 ThreadId(1) >> main-pre  3
[2026-01-18T11:55:06Z INFO  wingfoil] 0.200_850 ThreadId(7) >> mapper    30
[2026-01-18T11:55:06Z INFO  wingfoil] 0.200_521 ThreadId(1) >> main-post 30
[2026-01-18T11:55:06Z INFO  wingfoil] 0.300_160 ThreadId(6) >> producer  4
[2026-01-18T11:55:06Z INFO  wingfoil] 0.300_547 ThreadId(1) >> main-pre  4
[2026-01-18T11:55:06Z INFO  wingfoil] 0.300_888 ThreadId(7) >> mapper    40
[2026-01-18T11:55:06Z INFO  wingfoil] 0.300_547 ThreadId(1) >> main-post 40
[2026-01-18T11:55:06Z INFO  wingfoil] 0.409_299 ThreadId(6) >> producer  5
[2026-01-18T11:55:06Z INFO  wingfoil] 0.409_213 ThreadId(1) >> main-pre  5
[2026-01-18T11:55:06Z INFO  wingfoil] 0.409_645 ThreadId(7) >> mapper    50
[2026-01-18T11:55:06Z INFO  wingfoil] 0.409_761 ThreadId(1) >> main-post 50
[2026-01-18T11:55:06Z INFO  wingfoil] 0.502_253 ThreadId(6) >> producer  6
[2026-01-18T11:55:06Z INFO  wingfoil] 0.502_455 ThreadId(1) >> main-pre  6
[2026-01-18T11:55:06Z INFO  wingfoil] 0.502_782 ThreadId(7) >> mapper    60
[2026-01-18T11:55:06Z INFO  wingfoil] 0.503_107 ThreadId(1) >> main-post 60
[2026-01-18T11:55:06Z INFO  wingfoil] 0.601_341 ThreadId(6) >> producer  7
```

## Historical Mode Output

Log lines may appear out of order due to channel buffering between threads, but the engine time remains consistent.

```
[2026-01-18T11:58:32Z INFO  wingfoil] 0.000_000 ThreadId(6) >> producer  1
[2026-01-18T11:58:32Z INFO  wingfoil] 0.100_000 ThreadId(6) >> producer  2
[2026-01-18T11:58:32Z INFO  wingfoil] 0.200_000 ThreadId(6) >> producer  3
[2026-01-18T11:58:32Z INFO  wingfoil] 0.300_000 ThreadId(6) >> producer  4
[2026-01-18T11:58:32Z INFO  wingfoil] 0.400_000 ThreadId(6) >> producer  5
[2026-01-18T11:58:32Z INFO  wingfoil] 0.500_000 ThreadId(6) >> producer  6
[2026-01-18T11:58:32Z INFO  wingfoil] 0.600_000 ThreadId(6) >> producer  7
[2026-01-18T11:58:32Z INFO  wingfoil] 0.700_000 ThreadId(6) >> producer  8
[2026-01-18T11:58:32Z INFO  wingfoil] 0.000_000 ThreadId(1) >> main-pre  1
[2026-01-18T11:58:32Z INFO  wingfoil] 0.000_000 ThreadId(7) >> mapper    10
[2026-01-18T11:58:32Z INFO  wingfoil] 0.000_000 ThreadId(1) >> main-post 10
[2026-01-18T11:58:32Z INFO  wingfoil] 0.100_000 ThreadId(1) >> main-pre  2
[2026-01-18T11:58:32Z INFO  wingfoil] 0.100_000 ThreadId(7) >> mapper    20
[2026-01-18T11:58:32Z INFO  wingfoil] 0.100_000 ThreadId(1) >> main-post 20
[2026-01-18T11:58:32Z INFO  wingfoil] 0.200_000 ThreadId(1) >> main-pre  3
[2026-01-18T11:58:32Z INFO  wingfoil] 0.200_000 ThreadId(7) >> mapper    30
[2026-01-18T11:58:32Z INFO  wingfoil] 0.200_000 ThreadId(1) >> main-post 30
[2026-01-18T11:58:32Z INFO  wingfoil] 0.300_000 ThreadId(1) >> main-pre  4
[2026-01-18T11:58:32Z INFO  wingfoil] 0.300_000 ThreadId(7) >> mapper    40
[2026-01-18T11:58:32Z INFO  wingfoil] 0.300_000 ThreadId(1) >> main-post 40
[2026-01-18T11:58:32Z INFO  wingfoil] 0.400_000 ThreadId(1) >> main-pre  5
[2026-01-18T11:58:32Z INFO  wingfoil] 0.400_000 ThreadId(7) >> mapper    50
[2026-01-18T11:58:32Z INFO  wingfoil] 0.400_000 ThreadId(1) >> main-post 50
[2026-01-18T11:58:32Z INFO  wingfoil] 0.500_000 ThreadId(1) >> main-pre  6
[2026-01-18T11:58:32Z INFO  wingfoil] 0.500_000 ThreadId(7) >> mapper    60
[2026-01-18T11:58:32Z INFO  wingfoil] 0.500_000 ThreadId(1) >> main-post 60
[2026-01-18T11:58:32Z INFO  wingfoil] 0.600_000 ThreadId(1) >> main-pre  7
[2026-01-18T11:58:32Z INFO  wingfoil] 0.600_000 ThreadId(7) >> mapper    70
[2026-01-18T11:58:32Z INFO  wingfoil] 0.600_000 ThreadId(1) >> main-post 70
```

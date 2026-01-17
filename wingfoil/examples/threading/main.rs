use std::rc::Rc;
use std::thread;
use std::time::Duration;
use tinyvec::TinyVec;
use wingfoil::*;

/// This example demonstrates multi-threaded graph execution using
/// `producer()` and `mapper()` to offload work to worker threads.
///
/// - `producer()`: Spawns a sub-graph on a worker thread, returning values to the main graph
/// - `mapper()`: Processes incoming values on a worker thread
///
/// Both use kanal channels for inter-thread communication and work with
/// Historical and RealTime run modes.
fn main() {
    env_logger::init();

    let period = Duration::from_millis(50);
    let n_ticks = 10;
    let run_for = RunFor::Duration(period * n_ticks);

    println!("=== Threading Example ===\n");
    println!("Demonstrates producer() and mapper() for multi-threaded graph execution.\n");

    // Run in both modes to show they both work
    for run_mode in [RunMode::HistoricalFrom(NanoTime::ZERO), RunMode::RealTime] {
        println!("--- Running with {:?} ---\n", run_mode);
        run_example(run_mode, run_for, period);
        println!();
    }
}

fn run_example(run_mode: RunMode, run_for: RunFor, period: Duration) {
    // Helper to create a label with thread ID for logging
    let label = || {
        let thread_id = thread::current().id();
        format!("{:?}", thread_id)
    };

    // producer() spawns this sub-graph on a worker thread.
    // It generates a sequence of numbers 1, 2, 3, ...
    let produce_graph = move || {
        println!("[Producer] Starting on thread {:?}", thread::current().id());

        ticker(period)
            .count()
            .limit(10)
            .map(move |x| {
                println!(
                    "[Producer] Generated value {} on thread {:?}",
                    x,
                    thread::current().id()
                );
                x
            })
            .logged(&format!("producer:{}", label()), log::Level::Info)
    };

    // mapper() receives values wrapped in TinyVec<[TinyVec<[T; 1]>; 1]>
    // because producer() outputs TinyVec<[T; 1]> and mapper() wraps that again.
    let map_graph = move |src: Rc<dyn Stream<TinyVec<[TinyVec<[u64; 1]>; 1]>>>| {
        println!("[Mapper] Starting on thread {:?}", thread::current().id());

        src.map(move |xs| {
            // Flatten the nested TinyVecs and multiply by 10
            let result: Vec<u64> = xs.iter().flatten().map(|x| x * 10).collect();
            println!(
                "[Mapper] Transformed {:?} -> {:?} on thread {:?}",
                xs,
                result,
                thread::current().id()
            );
            result
        })
        .logged(&format!("mapper:{}", label()), log::Level::Info)
    };

    // Build and run the graph:
    // 1. producer() runs produce_graph on a worker thread
    // 2. mapper() runs map_graph on another worker thread
    // 3. The final map/accumulate runs on the main graph thread
    producer(produce_graph)
        .mapper(map_graph)
        .map(move |xs| {
            // Final transformation on main graph thread
            let result: Vec<u64> = xs.iter().flatten().map(|x| x * 10).collect();
            println!(
                "[Main] Final transform {:?} -> {:?} on thread {:?}",
                xs,
                result,
                thread::current().id()
            );
            result
        })
        .logged(&format!("main:{}", label()), log::Level::Info)
        .accumulate()
        .finally(|results, _| {
            let flat: Vec<u64> = results.into_iter().flatten().collect();
            println!("\n[Result] All values: {:?}", flat);
            println!(
                "[Result] Expected pattern: each input N becomes N*100 (10 from mapper, 10 from main)"
            );
        })
        .run(run_mode, run_for)
        .unwrap();
}

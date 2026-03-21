// Demonstrates O(2^N) depth-first execution with async streams.
//
// Each level fans the upstream broadcast out to two receivers and wires a
// combine_latest task that fires whenever EITHER receiver gets a value.
// One source send produces 2^N downstream wakeups across N levels.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tokio::runtime::Runtime;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

const CAPACITY: usize = 64;

/// Spawn a combine_latest task: reads from rx_left and rx_right, emits to tx
/// whenever either side updates (if both have a value). This mirrors rxrust's
/// combine_latest semantics and produces the same O(2^N) fan-out.
fn spawn_combine_latest(
    mut rx_left: broadcast::Receiver<u128>,
    mut rx_right: broadcast::Receiver<u128>,
    tx: broadcast::Sender<u128>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut latest_left: Option<u128> = None;
        let mut latest_right: Option<u128> = None;
        loop {
            tokio::select! {
                Ok(v) = rx_left.recv() => {
                    latest_left = Some(v);
                    if let (Some(l), Some(r)) = (latest_left, latest_right) {
                        let _ = tx.send(black_box(l + r));
                    }
                }
                Ok(v) = rx_right.recv() => {
                    latest_right = Some(v);
                    if let (Some(l), Some(r)) = (latest_left, latest_right) {
                        let _ = tx.send(black_box(l + r));
                    }
                }
                else => break,
            }
        }
    })
}

/// Build a chain of depth N. Returns (source_tx, leaf_rx, task_handles).
fn build_chain(
    depth: usize,
) -> (
    broadcast::Sender<u128>,
    broadcast::Receiver<u128>,
    Vec<JoinHandle<()>>,
) {
    let (source_tx, _) = broadcast::channel::<u128>(CAPACITY);
    let mut current_tx = source_tx.clone();
    let mut handles = Vec::new();

    for _ in 0..depth {
        let (next_tx, _) = broadcast::channel::<u128>(CAPACITY);
        let rx_left = current_tx.subscribe();
        let rx_right = current_tx.subscribe();
        handles.push(spawn_combine_latest(rx_left, rx_right, next_tx.clone()));
        current_tx = next_tx;
    }

    let leaf_rx = current_tx.subscribe();
    (source_tx, leaf_rx, handles)
}

fn bench_depth(crit: &mut Criterion, rt: &Runtime, depth: usize) {
    let (source_tx, mut leaf_rx, _handles) = rt.block_on(async { build_chain(depth) });

    // Warm up: send two values so all levels have an initial latest on both arms.
    rt.block_on(async {
        let _ = source_tx.send(1u128);
        let _ = source_tx.send(1u128);
        // Drain until we see output from the leaf.
        let _ = leaf_rx.recv().await;
    });

    crit.bench_function(&format!("depth_{depth}"), |b| {
        b.iter(|| {
            rt.block_on(async {
                let _ = source_tx.send(black_box(1u128));
                let _ = leaf_rx.recv().await;
            });
        });
    });
}

fn bench(crit: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    for depth in 1..=10 {
        bench_depth(crit, &rt, depth);
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);

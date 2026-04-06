# iceoryx2 Adapter Examples

[iceoryx2](https://github.com/eclipse-iceoryx/iceoryx2) is a zero-copy, lock-free inter-process communication (IPC) library. It uses shared memory to pass data between processes without serialization or kernel involvement, making it a good fit for latency-sensitive systems like market data distribution or robotics.

Key characteristics:
- **Zero-copy** — publishers write directly into shared memory; subscribers read in-place with no memcpy
- **Daemonless** — no central broker or media driver process required (unlike Aeron or the original iceoryx)
- **Lock-free** — wait-free algorithms on the hot path
- **Typed** — payload types must be `#[repr(C)]` and implement `ZeroCopySend` (no heap pointers, no `String`, no `Vec`)

## Polling Modes

The wingfoil adapter supports three subscriber polling modes, selected via `Iceoryx2Mode`:

| Mode | How it works | Latency | CPU |
|------|-------------|---------|-----|
| **Spin** | Polls iceoryx2 directly inside the graph `cycle()` loop | Lowest | Highest (burns one core) |
| **Threaded** | Polls in a dedicated background thread, delivers via channel | Medium (one channel hop) | Lower (10us yield when idle) |
| **Signaled** | Event-driven WaitSet — blocks until the publisher signals | Highest | Lowest (true blocking) |

The subscriber example lets you try all three:

```bash
cargo run --example iceoryx2_sub --features iceoryx2-beta -- spin
cargo run --example iceoryx2_sub --features iceoryx2-beta -- threaded
cargo run --example iceoryx2_sub --features iceoryx2-beta -- signaled
```

## Service Variants

The adapter supports two iceoryx2 service variants via `Iceoryx2ServiceVariant`:

- **Ipc** (default) — shared memory, for communication between separate processes
- **Local** — heap-based, for in-process use (useful for testing)

## Running the Examples

Start the publisher in one terminal, then the subscriber in another:

```bash
# Terminal 1: publisher
RUST_LOG=info cargo run --example iceoryx2_pub --features iceoryx2-beta

# Terminal 2: subscriber (pick a mode)
RUST_LOG=info cargo run --example iceoryx2_sub --features iceoryx2-beta -- spin
```

## Publisher

Publishes a `Counter` struct over shared memory every 100ms.

```rust
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend)]
struct Counter {
    seq: u64,
}

let upstream = ticker(period)
    .count()
    .map(|seq: u64| {
        let mut b = Burst::default();
        b.push(Counter { seq });
        b
    })
    .logged("pub", Info);

let pub_node = iceoryx2_pub(upstream, "wingfoil/examples/counter");

Graph::new(vec![pub_node], RunMode::RealTime, RunFor::Forever)
    .run()
    .unwrap();
```

## Subscriber

Subscribes to the counter service with a configurable polling mode.

```rust
let opts = Iceoryx2SubOpts {
    mode: Iceoryx2Mode::Spin, // or Threaded, Signaled
    ..Default::default()
};

let sub = iceoryx2_sub_opts::<Counter>("wingfoil/examples/counter", opts);
sub.collapse()
    .inspect(|c: &Counter| println!("received seq={}", c.seq))
    .logged("sub", Info)
    .run(RunMode::RealTime, RunFor::Forever)
    .unwrap();
```

## Zero-Copy Requirements

Payload types must be `#[repr(C)]`, self-contained (no heap allocations), and derive `ZeroCopySend`. For variable-length data, the adapter also provides a byte-slice API (`iceoryx2_pub_slice` / `iceoryx2_sub_slice`).

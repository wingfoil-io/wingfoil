# iceoryx2 Adapter Examples (wingfoil-next)

[iceoryx2](https://github.com/eclipse-iceoryx/iceoryx2) is a zero-copy, lock-free
inter-process communication (IPC) library. It uses shared memory to pass data
between processes without serialization or kernel involvement, making it a good
fit for latency-sensitive systems like market data distribution or robotics.

A port of the classic `wingfoil/examples/iceoryx2` examples onto the next engine.

Key characteristics:
- **Zero-copy** — publishers write directly into shared memory; subscribers read in-place with no memcpy
- **Daemonless** — no central broker or media driver process required (unlike Aeron or the original iceoryx)
- **Lock-free** — wait-free algorithms on the hot path
- **Typed** — payload types must be `#[repr(C)]` and implement `ZeroCopySend` (no heap pointers, no `String`, no `Vec`)

## Polling Modes

The adapter supports three subscriber polling modes, selected via `Iceoryx2Mode`:

| Mode | How it works | Latency | CPU |
|------|-------------|---------|-----|
| **Spin** | Polls iceoryx2 directly in the graph cycle (a busy-spin `custom_node`) | Lowest | Highest (burns one core) |
| **Threaded** | Polls in a dedicated background thread, delivers via the channel layer | Medium (one channel hop) | Lower (10 µs yield when idle) |
| **Signaled** | Event-driven WaitSet — blocks until the publisher signals | Highest | Lowest (true blocking) |

The subscriber example lets you try all three:

```bash
cargo run -p wingfoil-next --example iceoryx2_sub --features iceoryx2 -- spin
cargo run -p wingfoil-next --example iceoryx2_sub --features iceoryx2 -- threaded
cargo run -p wingfoil-next --example iceoryx2_sub --features iceoryx2 -- signaled
```

## Service Variants

Two iceoryx2 service variants, via `Iceoryx2ServiceVariant`:

- **Ipc** (default) — shared memory, for communication between separate processes
- **Local** — heap-based, for in-process use (what the adapter's own tests use)

## Setup

Nothing to install: iceoryx2 is daemonless. `Ipc` needs a writable `/dev/shm`
(standard on Linux); `Local` needs nothing at all.

## Run

Start the publisher in one terminal, then the subscriber in another:

```bash
# Terminal 1: publisher
RUST_LOG=info cargo run -p wingfoil-next --example iceoryx2_pub --features iceoryx2

# Terminal 2: subscriber (pick a mode)
RUST_LOG=info cargo run -p wingfoil-next --example iceoryx2_sub --features iceoryx2 -- spin
```

## Code

### Publisher

Publishes a `Counter` struct over shared memory every 100 ms.

```rust
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend)]
struct Counter {
    seq: u64,
}

let g = GraphBuilder::new();
let _publisher = g
    .ticker(Duration::from_millis(100))
    .count()
    .map(|seq: &u64| burst![Counter { seq: *seq }])
    .logged("pub", Info)
    .iceoryx2_pub("wingfoil/examples/counter");

g.build().run(RunMode::RealTime, RunFor::Forever)?;
```

### Subscriber

Subscribes to the counter service with a configurable polling mode.

```rust
let opts = Iceoryx2SubOpts {
    mode: Iceoryx2Mode::Spin, // or Threaded, Signaled
    ..Default::default()
};

let g = GraphBuilder::new();
let _sub = iceoryx2_sub_opts::<Counter>(&g, RunMode::RealTime, "wingfoil/examples/counter", opts)?
    .collapse()
    .inspect(|c: &Counter| println!("received seq={}", c.seq))
    .logged("sub", Info);

g.build().run(RunMode::RealTime, RunFor::Forever)?;
```

## Output

```
received seq=1
received seq=2
received seq=3
...
```

## Zero-Copy Requirements

Payload types must be `#[repr(C)]`, self-contained (no heap allocations), and
derive `ZeroCopySend`. For variable-length data the adapter also provides a
byte-slice API (`Iceoryx2SliceSinkOps::iceoryx2_pub_slice` / `iceoryx2_sub_slice`),
and `FixedBytes<N>` carries bounded byte payloads through the typed API.

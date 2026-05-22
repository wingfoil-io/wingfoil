# LinkedIn — Iceoryx2 adapter

## Post

We added an Iceoryx2 adapter to Wingfoil last week, behind the `iceoryx2-beta` feature flag.

If you haven't run into Iceoryx2 before: it's a Linux IPC layer that moves data between processes via shared memory, so there's no copy and no kernel round-trip. Useful when two processes on the same box need to talk to each other faster than a socket can.

The adapter is two nodes — `iceoryx2_pub` and `iceoryx2_sub`. Payloads have to be `#[repr(C)]` and `ZeroCopySend`, which is a real constraint (no `String`, no `Vec`), but the compiler holds you to it.

There are three polling modes depending on how much CPU you're willing to spend:
- Spin on the cycle (~1–5 µs, pins a core)
- Background thread that yields every 10 µs
- Event-driven WaitSet for the cases where you'd rather block

We've been using it to split an order gateway from a risk check that used to live in the same binary. Worth a look if you're doing something similar.

## First comment

- Adapter source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/iceoryx2
- Pub example: https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/iceoryx2/pub.rs
- Sub example: https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/iceoryx2/sub.rs
- Iceoryx2 project: https://github.com/eclipse-iceoryx/iceoryx2

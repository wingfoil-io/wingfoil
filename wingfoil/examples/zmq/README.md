# ZMQ Adapter Example

Demonstrates seed-based service discovery: a seed node acts as a lightweight
name registry so subscribers never need to hardcode publisher addresses. The
publisher registers itself with the seed on startup; the subscriber queries the
seed at construction time, then connects to the publisher directly over a normal
ZMQ PUB/SUB socket.

## Setup

No broker process is required. The `zmq-beta` feature bundles `libzmq` at build
time via `zeromq-src`.

## Run

```sh
RUST_LOG=info cargo run --example zmq --features zmq-beta
```

## Code

See [`main.rs`](main.rs).

Three components run in the same process (in a real deployment each would be
separate):

1. **Seed** — bound on `tcp://127.0.0.1:7777`; stops when dropped.
2. **Publisher** — binds a PUB socket on port 7778, registers as `"quotes"` with the seed.
3. **Subscriber** — queries the seed for `"quotes"`, receives the resolved address, connects.

## Output

```
[INFO  pub] 1
[INFO  pub] 2
[INFO  sub] [1]
[INFO  pub] 3
[INFO  sub] [2, 3]
...
received 28 values: [1, 2, 3, 4, ...]
```

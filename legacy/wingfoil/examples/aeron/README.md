# Aeron Adapter Example

Demonstrates a round-trip with the Aeron adapter: an `i64` publisher offers
values over an `aeron:ipc` channel, and a subscriber receives them back —
shown with both polling modes:

1. **Spin** *(primary)* — the subscriber polls Aeron inside the graph `cycle()`
   on the graph thread (zero thread-crossing latency).
2. **Threaded** *(secondary)* — the subscriber polls on a background thread and
   delivers fragments to the graph over a channel.

## Setup

The Aeron adapter needs a running **media driver**. The simplest local option
is the C++ driver in a container, sharing `/dev/shm` so the host process
connects over `aeron:ipc`:

```sh
docker run --rm \
  -v /dev/shm:/dev/shm \
  -e AERON_DIR=/dev/shm/aeron-default \
  neomantra/aeron-cpp-debian
```

Alternatively run the Java driver from an Aeron release:

```sh
java -cp aeron-all-*.jar io.aeron.driver.MediaDriver
```

System dependencies for the `aeron` (rusteron) backend: `cmake` (≥3.30),
`clang`, `libclang-dev`, `uuid-dev`, `libbsd-dev`.

## Run

```sh
cargo run --example aeron --features aeron
```

## Code

See [`main.rs`](main.rs).

## Output

```
=== Spin subscriber ===
spin received: 0
spin received: 1
spin received: 2
...

=== Threaded subscriber ===
threaded received: 0
threaded received: 1
...
```

(Exact values depend on timing — the publisher emits a small ramp and the
subscriber prints whatever has arrived each cycle.)

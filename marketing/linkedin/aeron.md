# LinkedIn — Aeron adapter

## Post

We added an Aeron adapter to Wingfoil, behind the `aeron` feature flag.

Aeron is the low-latency UDP/IPC message transport a lot of trading systems already standardise on, so wiring it into the graph directly was an obvious one. The adapter is two nodes: `aeron_sub_fragment` consumes a channel and emits a `Burst<T>` into the graph; `aeron_pub` (fluent on a stream) offers each burst back out over a channel.

Two polling modes, same trade-off as our other low-latency adapters:
- Spin — poll Aeron inside the graph `cycle()` on the graph thread. Zero thread-crossing latency, burns a core.
- Threaded — poll on a background thread and deliver over a channel. One channel hop, frees the graph thread.

The part we're happiest with is the status side-channel. `aeron_sub_fragment_with_status` / `aeron_pub_with_status` hand you a second stream of `AeronStatus` (Connected / Disconnected / BackPressured / Closed) alongside the data, so backpressure and disconnects are just events in the graph — you can wire them into a circuit breaker rather than discovering them in a log.

Two backends to pick from: `aeron` uses the production rusteron C++ client; `aeron-rs-beta` is a pure-Rust backend for when you'd rather not pull in cmake and clang. Either way you point it at a running media driver.

Available in Rust and via the PyO3 Python bindings.

## First comment

- Adapter source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/aeron
- Example: https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/aeron/main.rs
- Status circuit breaker example: https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/aeron/status_circuit_breaker.rs
- Aeron: https://github.com/aeron-io/aeron

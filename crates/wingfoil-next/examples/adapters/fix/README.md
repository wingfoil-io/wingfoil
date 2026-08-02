# FIX Adapter Example (wingfoil-next)

FIX 4.4 loopback — an acceptor and an initiator running in the same process, on
the next engine. A self-contained port of the classic
`wingfoil/examples/fix/fix_loopback`.

**No external FIX engine is required.** That is what makes this the one FIX
example worth shipping runnable; see [Scope](#scope) for the four classic
programs that are deliberately not ported.

Demonstrates:

- `fix_accept` — the acceptor side
- `fix_connect` with `FixPollMode::AlwaysSpin` — lowest-latency, graph-driven
- ordinary stream operators (`map` / `fold` / `map_filter` / `logged`) applied to
  the FIX session-status and data streams

## Run

```sh
RUST_LOG=info cargo run -p wingfoil-next --example fix_adapter --features fix
```

`RUST_LOG=info` matters — this example reports through the `logged` tap rather
than `println!`, so without it you will see nothing. See
[`core/tracing`](../../core/tracing/) for what `logged` is.

## Code

The acceptor is wired **first**, deliberately: its listener binds at graph
`start()`, which has to happen before the spin initiator's synchronous connect
runs. Wiring order is the only thing sequencing them.

```rust
// ── Acceptor side (wired first so its listener is bound before connect) ──
let (acc_data, acc_status) = fix_accept(
    &g, RunMode::RealTime, port, "ACCEPTOR", "INITIATOR", FixPollMode::AlwaysSpin,
)?;

// ── Initiator side ──
let (init_data, init_status) = fix_connect(
    &g, RunMode::RealTime, "127.0.0.1", port, "INITIATOR", "ACCEPTOR", FixPollMode::AlwaysSpin,
)?;

// Running count of FIX data messages received by the acceptor.
let _acc_msg_count = acc_data
    .map(|burst: &_| burst.len())
    .fold(0usize, |acc, n| *acc += n)
    .logged("acceptor-msg-count", Info);

// Initiator status filtered to LoggedIn transitions only.
let _init_logged_in = init_status
    .map_filter(|burst: &_| {
        let logged_in = burst.contains(&FixSessionStatus::LoggedIn);
        (logged_in, logged_in)
    })
    .logged("initiator-logon", Info);
```

Both sides return a `(data, status)` pair, the same shape as
[`zmq`](../zmq/) — session state is a stream, so a logon, a logout, or a
sequence-number reset is an ordinary graph event you can fold or gate on.

`FixPollMode::AlwaysSpin` busy-polls the socket from the graph thread for the
lowest possible latency. The alternative modes hand polling to a background
thread; see the adapter's module docs for the trade-off.

## Output

Log lines through the `logged` taps:

```text
[INFO  wingfoil] Starting FIX loopback on port 19876
[INFO  wingfoil] acceptor-status: [Connected]
[INFO  wingfoil] initiator-logon: true
[INFO  wingfoil] acceptor-msg-count: 1
[INFO  wingfoil] Done.
```

## Scope

Of the classic tree's five `wingfoil/examples/fix/` programs, only `fix_loopback`
is self-contained, so it is the only one ported:

| Classic program | Why not ported |
|---|---|
| `fix_client` | needs a separate counterparty process |
| `fix_echo_server` | needs a separate counterparty process |
| `lmax_demo` | requires a (free) LMAX London Demo account and credentials |
| `lmax_instruments` | same |

All the API they exercise — `fix_connect`, `fix_accept`, `fix_connect_tls`,
`fix_sub`, `fix_send` — **is** ported, and is covered by the adapter tests. The
live LMAX path also runs in [`showcase/latency_e2e`](../../showcase/latency_e2e/),
which drives real market data over FIX/TLS.

## See also

- [`showcase/latency_e2e`](../../showcase/latency_e2e/) — FIX/TLS to LMAX as one
  hop of a nine-stage end-to-end latency demo.
- [`core/tracing`](../../core/tracing/) — the `logged` tap this example reports through.

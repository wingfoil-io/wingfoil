# FIX Adapter Examples

Demonstrates the wingfoil FIX (Financial Information eXchange) protocol adapter
with both self-contained loopback examples and live exchange connectivity.

## Examples

### fix_loopback

Self-contained acceptor + initiator in the same process. No external FIX engine needed.

```sh
RUST_LOG=info cargo run --example fix_loopback --features fix
```

### fix_client

Connects to an external FIX acceptor and logs incoming messages.

```sh
RUST_LOG=info cargo run --example fix_client --features fix
```

### fix_echo_server

Accepts a FIX initiator connection and echoes back received application messages.

```sh
RUST_LOG=info cargo run --example fix_echo_server --features fix
```

### lmax_demo

Connects to the LMAX London Demo market data gateway via TLS, subscribes to
AVAX/USD, and prints live bid/ask updates.

Requires a free demo account: https://register.london-demo.lmax.com/registration/LMB/

```sh
RUST_LOG=info LMAX_USERNAME=your_username LMAX_PASSWORD=your_password \
  cargo run --example lmax_demo --features fix
```

### lmax_instruments

Queries the LMAX London Demo for available instrument details via SecurityListRequest.

```sh
RUST_LOG=info LMAX_USERNAME=your_username LMAX_PASSWORD=your_password \
  cargo run --example lmax_instruments --features fix
```

## See Also

- [LMAX.md](LMAX.md) — LMAX-specific configuration notes

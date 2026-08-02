# LMAX London Demo — Setup & Testing

## 1. Register

Go to <https://register.london-demo.lmax.com/registration/LMB/> and complete the
free registration form (name, email, phone number). Credentials are sent by email.

## 2. Export credentials

```sh
export LMAX_USERNAME=<your username>
export LMAX_PASSWORD=<your password>
```

## 3. Run the integration tests

```sh
cargo test --features fix-integration-test -p wingfoil \
  -- lmax --nocapture --test-threads=1
```

Two tests are included:

| Test | What it checks |
|---|---|
| `lmax_logon` | TLS handshake + FIX logon succeeds (`LoggedIn` status received within 10 s) |
| `lmax_market_data` | MarketDataRequest for EUR/USD (instrument 4001) returns at least one `W` or `X` message within 20 s |

Both tests skip gracefully (with a `SKIP` notice) if the env vars are not set, so
they are safe to include in CI pipelines where credentials are absent.

## 4. Run the demo example

```sh
RUST_LOG=info cargo run --example lmax_demo --features fix
```

Connects to the market data session, subscribes to EUR/USD, and prints incoming
FIX messages for 60 seconds.

## Session parameters

| Parameter | Market Data | Order Routing |
|---|---|---|
| Host | `fix-marketdata.london-demo.lmax.com` | `fix-order.london-demo.lmax.com` |
| Port | `443` | `443` |
| TargetCompID | `LMXBDM` | `LMXBD` |
| SenderCompID | your username | your username |
| Protocol | FIX 4.4 over TLS | FIX 4.4 over TLS |

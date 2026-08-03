# Showcase Examples

Multi-process, end-to-end demonstrations. Unlike [`core/`](../core/) and
[`adapters/`](../adapters/) — which each isolate one idea — these compose several
adapters and several processes into a system, and measure it.

Both are about **latency**: not "does it work" but "where did the microseconds
go".

| Example | Processes | Transport | What it demonstrates |
|---|---|---|---|
| [`latency`](latency/) | 2 | iceoryx2 | Per-hop latency stamping across a shared-memory hop. |
| [`latency_e2e`](latency_e2e/) | 3 + observability stack | WebSocket → iceoryx2 → FIX/TLS | Nine stages, browser to venue and back, with live dashboards. |

## `latency` — per-hop stamping

A publisher and a subscriber talking over iceoryx2. The point is the latency
infrastructure rather than the transport: `latency_stages!` declares the schema,
`Traced<T, L>` carries the stamps with the payload, `.stamp::<Stage>()` records a
hop, and `latency_report` renders the distribution.

Run the **subscriber first** — it creates the shared-memory service the publisher
attaches to:

```sh
cargo run --manifest-path crates/wingfoil/Cargo.toml --release --example latency_sub --features iceoryx2
cargo run --manifest-path crates/wingfoil/Cargo.toml --release --example latency_pub --features iceoryx2
```

`shared.rs` is `#[path]`-included by both binaries so the two processes agree on
the payload type and the stage schema — they must, since the stamps travel in the
payload.

## `latency_e2e` — the full stack

A browser sends an order over WebSocket to `ws_server`, which forwards it over
iceoryx2 to `fix_gw`, which prices it against live LMAX market data over FIX/TLS
and sends a fill back along the same path. Nine stages are stamped end to end.

Alongside the two binaries it carries a complete observability stack — Prometheus
scraping, Grafana dashboards, Tempo for traces, a browser client, five
Dockerfiles, and Pulumi stacks for three deployment shapes (Fargate, EC2 Spot,
bare metal).

```sh
docker compose -f next/crates/wingfoil/examples/showcase/latency_e2e/docker-compose.yml up -d
```

See [`latency_e2e/README.md`](latency_e2e/README.md) for the full run-through and
[`DOCKER_BUILD.md`](latency_e2e/DOCKER_BUILD.md) for building and pushing the
images.

## Use `--release`

Both examples measure latency. A debug build's numbers are meaningless — always
pass `--release`.

## Elsewhere

- [`../adapters/iceoryx2/`](../adapters/iceoryx2/) — the transport on its own.
- [`../adapters/fix/`](../adapters/fix/) — the FIX side, self-contained and needing no venue.
- [`../adapters/prometheus/`](../adapters/prometheus/), [`../adapters/otlp/`](../adapters/otlp/) — the exporters this stack wires together.
- [`../../benches/`](../../benches/) — microbenchmarks, where these are system measurements.

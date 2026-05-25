# Wingfoil marketing drafts

## LinkedIn posting order

| # | Topic | File | Angle |
|---|-------|------|-------|
| 1 | Bun rewrite / agentic coding | [`linkedin/rust-agentic-coding.md`](linkedin/rust-agentic-coding.md) | Topical opener — leads with the Bun-to-Rust merge, makes the case for Rust as an agent-friendly language. |
| 2 | Iceoryx2 adapter | [`linkedin/iceoryx2.md`](linkedin/iceoryx2.md) | Zero-copy shared-memory IPC for splitting a graph across processes on one box. |
| 3 | Telemetry | [`linkedin/telemetry.md`](linkedin/telemetry.md) | Prometheus + OTLP + tracing spans, all opt-in. |
| 4 | `/new-adapter` skill | [`linkedin/new-adapter-skill.md`](linkedin/new-adapter-skill.md) | Month-to-weekend speedup for new I/O adapters; consistency as a quality multiplier. |
| 5 | WebSocket + JS client | [`linkedin/websocket-js-client.md`](linkedin/websocket-js-client.md) | Streaming graph values to browsers with shared Rust→WASM wire types. |
| 6 | Kafka adapter | [`linkedin/kafka.md`](linkedin/kafka.md) | Two nodes, multi-topic publish, per-burst delivery via `FuturesUnordered`. |
| 7 | KDB+ follow-up | [`linkedin/kdb-group-followup.md`](linkedin/kdb-group-followup.md) | Deeper-dive companion for the KDB LinkedIn group — caller-owned q, time slicing, `kdb_read_cached`. |

## Other drafts

| File | Destination | Notes |
|------|-------------|-------|
| [`github/iceoryx2-discussion.md`](github/iceoryx2-discussion.md) | iceoryx2 GitHub discussions board | Long-form Show-and-Tell companion to the LinkedIn iceoryx2 post. Pair it with #2 above. |

## Format convention

Every LinkedIn draft has two sections:

- **Post** — the main body.
- **First comment** — links, posted as the first comment under the post to avoid suppressing reach.

## Before posting

- Verify the Bun-rewrite numbers in post #1 (~13k unsafe blocks, uv ~70) — these may shift as the Bun team refactors.
- Spot-check every URL in the "First comment" sections — file and folder paths were lifted from the working tree and not all have been visited.

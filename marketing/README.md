# Wingfoil marketing drafts

## LinkedIn posting order

| # | Topic | File | Angle |
|---|-------|------|-------|
| 1 | Bun rewrite / agentic coding | [`linkedin/rust-agentic-coding.md`](linkedin/rust-agentic-coding.md) | Topical opener — leads with the Bun-to-Rust merge, makes the case for Rust as an agent-friendly language. |
| 2 | Iceoryx2 adapter | [`linkedin/iceoryx2.md`](linkedin/iceoryx2.md) | Zero-copy shared-memory IPC for splitting a graph across processes on one box. |
| 3 | Telemetry | [`linkedin/telemetry.md`](linkedin/telemetry.md) | Prometheus + OTLP + tracing spans, all opt-in. |
| 4 | WebSocket + JS client | [`linkedin/websocket-js-client.md`](linkedin/websocket-js-client.md) | Streaming graph values to browsers with shared Rust→WASM wire types. |
| 5 | Kafka adapter | [`linkedin/kafka.md`](linkedin/kafka.md) | Two nodes, multi-topic publish, per-burst delivery via `FuturesUnordered`. |
| 6 | KDB+ follow-up | [`linkedin/kdb-group-followup.md`](linkedin/kdb-group-followup.md) | Deeper-dive companion for the KDB LinkedIn group — caller-owned q, time slicing, `kdb_read_cached`. |
| 7 | Fluvio adapter | [`linkedin/fluvio.md`](linkedin/fluvio.md) | Two nodes, per-burst flush, absolute-offset resume, SC+SPU cluster shape. |
| 8 | `/new-adapter` call for contributors | [`linkedin/new-adapter-skill-contributors.md`](linkedin/new-adapter-skill-contributors.md) | Month-to-weekend speedup for new I/O adapters; consistency as a quality multiplier, framed as an open call to build whatever adapter you need, with specced-out issues to pick up. |
| 9 | Three-language stack | [`linkedin/three-language-stack.md`](linkedin/three-language-stack.md) | Rust/Python/TS is enough for greenfield work (Rust core, PyO3, JS/TS edge); C++/Java/C#/Go on new projects driven by codebase inertia and talent constraints, not technical merit. Deliberately provocative — ends by daring readers to name the counterexample. |

## Other drafts

| File | Destination | Notes |
|------|-------------|-------|
| [`github/iceoryx2-discussion.md`](github/iceoryx2-discussion.md) | iceoryx2 GitHub discussions board | Long-form Show-and-Tell companion to the LinkedIn iceoryx2 post. Pair it with #2 above. |
| [`github/fluvio-discussion.md`](github/fluvio-discussion.md) | Fluvio GitHub discussions board | Show-and-Tell companion to the LinkedIn Fluvio post. Notes SC+SPU gotchas and asks about single-container test setup. |

## Format convention

Every LinkedIn draft has two sections:

- **Post** — the main body.
- **First comment** — links, posted as the first comment under the post to avoid suppressing reach.

## Before posting

- Spot-check every URL in the "First comment" sections — file and folder paths were lifted from the working tree and not all have been visited.
- Post #6 (KDB+ follow-up) references q syntax with backticks that LinkedIn will eat. Plan: post the q snippet as a screenshot inline rather than rewording.

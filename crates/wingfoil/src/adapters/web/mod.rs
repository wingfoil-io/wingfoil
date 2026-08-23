//! web adapter — bidirectional streaming between a wingfoil graph and one
//! or more browsers over WebSocket. It ports the legacy
//! `wingfoil::adapters::web` module onto the Op model.
//!
//! The [`WebServer`] hosts an HTTP + WebSocket listener on its own dedicated
//! tokio runtime (the same pattern as the
//! [`prometheus`](crate::adapters::prometheus) exporter). Graph nodes register
//! topics on the server:
//!
//! - [`WebSinkOps::web_pub`] — publishes a stream's values to every client
//!   subscribed to the topic.
//! - [`web_sub`] — exposes frames sent by the browser on a topic as a source
//!   stream.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`statistics`](crate::adapters::statistics)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude). Bring in
//! what you need explicitly:
//!
//! - **Source** — the free builder function [`web_sub`] on a
//!   [`GraphBuilder`](crate::fluent::GraphBuilder), emitting
//!   `Stream<Burst<T>>`.
//! - **Delivery** — [`WebServerBuilder::delivery`] chooses what happens when a
//!   client cannot keep up; see [`Delivery`] and *Historical mode* below.
//! - **Sinks** — the [`WebSinkOps`] extension trait on any `Stream<T>` whose
//!   value serializes (one scalar payload per frame), and
//!   [`WebBurstSinkOps`] on `Stream<Burst<T>>` (the whole same-instant group as
//!   one atomic array frame), enabled with
//!   `use wingfoil::adapters::web::{WebSinkOps, WebBurstSinkOps};`.
//!
//! # Wire format
//!
//! Binary frames use [`bincode`](https://docs.rs/bincode) by default; pass
//! [`CodecKind::Json`] for a human-readable mode useful in browser devtools.
//! The shared [`Envelope`] / [`ControlMessage`] types are declared in the
//! [`wingfoil-wire-types`](wingfoil_wire_types) crate, so the server, the
//! `wingfoil-wasm` browser client, and `@wingfoil/client` share one source of
//! truth — **the wire protocol is engine-agnostic and unchanged by this port**,
//! which is what lets `wingfoil-js` stay untouched.
//!
//! The control topic `"$ctrl"` carries `Hello { codec, version }` (server →
//! client on upgrade), `Subscribe`/`Unsubscribe { topics }` (client → server),
//! and `Complete { topic }` (server → client at end-of-stream).
//!
//! ## A browser peer needs [`CodecKind::Json`]
//!
//! bincode is schema-driven and non-self-describing: encoding *and* decoding
//! a payload require the Rust type. A browser does not have it, so under the
//! default [`CodecKind::Bincode`] a JS value can only be offered as a
//! schema-less map — which `deserialize_struct` reads as **silent garbage**,
//! not as an error — and a server-encoded payload cannot be decoded in the
//! browser at all. `@wingfoil/client` therefore rejects data payloads under
//! bincode outright (`encodePayload` / `decodePayload` throw), so
//! **`.codec(CodecKind::Json)` whenever a browser publishes to or subscribes
//! to this server**. Envelope and `$ctrl` frames have fixed shapes both sides
//! know and are unaffected; a Rust peer, which does have the schema, can use
//! bincode freely. A **Python** peer cannot be lumped in with it: the binding
//! marshals through `serde_json::Value`, so it has no more schema than the
//! browser does. `WebServer.sub` rejects bincode for that reason, and a
//! Python `pub` is only bincode-safe against a peer whose type matches the
//! value's own shape — see `wingfoil-python`'s `adapters::web` module docs for
//! the peer/codec table.
//!
//! # Publishing
//!
//! ```ignore
//! use std::time::Duration;
//! use wingfoil::{RunFor, RunMode};
//! use wingfoil::adapters::web::{WebServer, WebSinkOps};
//! use wingfoil::prelude::*;
//!
//! let server = WebServer::bind("127.0.0.1:0").start()?;
//! println!("open ws://127.0.0.1:{}/ws", server.port());
//!
//! let g = GraphBuilder::new();
//! let _pub = g.ticker(Duration::from_millis(10)).count().web_pub(&server, "tick")?;
//! g.build().run(RunMode::RealTime, RunFor::Forever)?;
//! ```
//!
//! # Subscribing (browser → graph)
//!
//! ```ignore
//! use wingfoil::adapters::web::{WebServer, web_sub};
//! use wingfoil::prelude::*;
//!
//! let server = WebServer::bind("127.0.0.1:0").start()?;
//! let g = GraphBuilder::new();
//! let clicks = web_sub::<u32>(&g, &server, "ui_events")?;
//! // Print the whole burst — every click that arrived this cycle. (Don't
//! // `.collapse()` an event stream: it keeps only the burst's last value.)
//! let _sink = clicks.print();
//! ```
//!
//! # Serving a static UI bundle
//!
//! ```ignore
//! let server = WebServer::bind("127.0.0.1:3000")
//!     .serve_static("./js/dist")
//!     .start()?;
//! ```
//!
//! # HTTPS / WSS (rustls)
//!
//! Enable the `web-tls` cargo feature and chain `.tls(cert, key)` onto the
//! builder. Cert and key are PEM files on disk; the rustls crypto provider is
//! `ring`, matching the [`fix`](crate::adapters::fix) adapter. Clients must
//! connect via `https://` / `wss://`. The `wingfoil-js` browser client honours
//! `location.protocol`, so the only client-side change is loading the page over
//! HTTPS.
//!
//! # Historical mode
//!
//! There are two ways to run a graph under
//! [`RunMode::HistoricalFrom`](wingfoil::RunMode::HistoricalFrom):
//!
//! - **Stream the replay to browsers** — use the normal
//!   [`WebServerBuilder::start`]. `web_pub` streams a historical replay's values
//!   out exactly as it does in real time, which is what powers browser-side
//!   visualisation of a backtest or slow computation. When the run ends,
//!   subscribed clients receive a [`ControlMessage::Complete`] end-of-stream
//!   marker (surfaced by `@wingfoil/client` as `onComplete`). `web_sub` yields
//!   an empty source in historical mode — live browser input has no place in a
//!   deterministic replay, and an open listener would block the run waiting for
//!   frames that never come.
//! - **No server at all** — use [`WebServerBuilder::start_historical`], which
//!   binds no TCP port and makes both `web_pub` and `web_sub` no-ops, so a
//!   backtest that does not want a server can run the same graph unmodified.
//!
//! ## Who waits for whom: [`Delivery`]
//!
//! Real time and historical want opposite answers, so this is a policy on the
//! builder rather than a constant. [`Delivery::Auto`] — the default — picks per
//! run mode, and it is almost always the right choice:
//!
//! - **Real time is lossy**, unchanged and deliberately so. A client that falls
//!   behind is already showing stale data, and the only alternative to dropping
//!   frames is stalling the graph — a frozen browser tab must never
//!   back-pressure a live system.
//! - **A historical replay is lossless.** There is no live clock to fall behind,
//!   so dropping frames does not keep up with anything; it corrupts the replay
//!   the browser is drawing. The publisher paces itself to the slowest
//!   subscribed client, so the whole replay arrives in order.
//!
//! With **no** subscribers nothing is ever waited on, so an unwatched replay
//! runs at full speed; with several, the pace is the slowest of them. Being
//! held up by a genuinely slow client is the contract, and why lossless is not
//! the real-time default. [`Delivery::Lossy`] forces the never-block behaviour
//! in both run modes.
//!
//! "Slowest" is bounded, because otherwise it would include *gone*: a half-open
//! peer never closes its socket, so an unbounded wait would park the graph until
//! TCP keepalive noticed. A subscriber that accepts nothing for
//! [`WebServerBuilder::lossless_stall_timeout`] (default 30 s) is withdrawn, and
//! its connection is **closed** so a client that does come back sees the close
//! and reconnects rather than holding a socket that will never carry another
//! frame. A live client cannot trip it — the wait is for one slot in a
//! 1024-deep queue.
//!
//! ## Runtime requirement (a `block_on` footgun)
//!
//! `web_pub` drives its encode + delivery off the graph thread with
//! [`consume_async_bursts`](crate::async_source::consume_async_bursts), which uses
//! [`Handle::block_on`](tokio::runtime::Handle::block_on) on the graph thread
//! for back-pressure and the teardown flush. So **the graph must be built, run,
//! and dropped from a non-async thread** (`main`, a `#[test]` fn). The HTTP
//! server itself runs on its own thread and its own runtime, independent of the
//! graph's.
//!
//! # Deviations from legacy
//!
//! Every legacy *capability* (both codecs, the v2 control plane, the static
//! file server, TLS, the historical streaming path with its `Complete` marker,
//! the historical no-op server, and the lossy never-back-pressuring transport)
//! is preserved, and the **wire format is byte-identical** — the shared
//! `wingfoil-wire-types` crate is reused as-is. The surface differs in these
//! deliberate ways:
//!
//! 1. **The source takes a [`GraphBuilder`](crate::fluent::GraphBuilder) and
//!    returns [`Result`](anyhow::Result).** Every wingfoil source wires on the
//!    builder; the `Result` covers the graph's tokio-runtime creation.
//!    Historical mode is *not* rejected at wiring (unlike the live `_sub`
//!    sources of register B2): `web_sub` is finite under historical replay —
//!    it yields an immediately-ending empty stream, exactly as legacy does.
//! 2. **The sink is a trait only.** Legacy exposed both a free `web_pub`
//!    function and a `WebPubOperators` trait; wingfoil folds the entry point into
//!    the [`WebSinkOps`] trait, per the sink-as-trait convention shared with
//!    [`lines`](crate::adapters::lines) / [`csv`](crate::adapters::csv) /
//!    [`kafka`](crate::adapters::kafka), and it returns the sink `Stream<()>`
//!    rather than an `Rc<dyn Node>`.
//! 3. **Two burst overloads are added**, both on [`WebBurstSinkOps`] rather
//!    than as second impls of [`WebSinkOps`] — `Burst`/`TinyVec` is not
//!    `Serialize`, and `WebSinkOps` is not generic over its payload, so
//!    `impl for Stream<T>` and `impl for Stream<Burst<T>>` would collide on
//!    coherence:
//!    - [`WebBurstSinkOps::web_pub_bursts`] publishes the whole same-instant
//!      group as **one atomic array frame**. Legacy could only do this by
//!      mapping `Burst<T>` to `Vec<T>` by hand; this does that conversion
//!      internally and produces byte-identical frames. The manual
//!      `.map(|b| b.to_vec()).web_pub(..)` route still works.
//!    - [`WebBurstSinkOps::web_pub_each`] publishes **one frame per value**,
//!      byte-identical to what [`WebSinkOps::web_pub`] emits for that value.
//!      It exists so a pipeline can stay burst-shaped end to end without
//!      changing the wire format: the alternative, `collapse()` before
//!      `web_pub`, keeps only the burst's last value and silently drops the
//!      rest — data loss that only appears once a producer outruns the graph
//!      cycle. Legacy has no equivalent.
//!
//!    The pair fixes the tree-wide suffix convention: `_each` means *per
//!    value in the burst* (here and on
//!    [`stamp_each`](crate::latency::LatencyBurstStreamOps::stamp_each)),
//!    `_bursts` means *the whole group as one atomic unit*.
//! 4. **`Complete` is emitted from the sink's teardown**, not from the consumer
//!    noticing its source ended. Wingfoil's [`consume_async_bursts`](crate::async_source::consume_async_bursts)
//!    hands back a `flush` teardown; `web_pub` chains its own `finally` that
//!    flushes every queued frame, joins the consumer, and *then* delivers
//!    `Complete { topic }` through the same fan-out — so the marker still
//!    arrives strictly after the last data frame, on the topic's own path, for
//!    both a finite `RunFor` and the end of a historical replay.
//! 5. **The envelope is encoded off the graph thread**, inside the
//!    `consume_async_bursts` consumer, as legacy did — the graph thread only clones
//!    the `(time, value)` pair into the sink channel. The fan-out registry is
//!    behind a mutex, so it must not be touched from a cycle (the
//!    no-locks-on-the-graph-path invariant).
//!
//! 6. **A historical replay is delivered losslessly by default, bounded by a
//!    stall timeout.** Legacy has one
//!    transport behaviour in both run modes — a fan-out that cannot block its
//!    sender, plus a `try_send` that drops on a full outbound queue — so a
//!    backtest running at CPU speed outruns any socket and the browser draws a
//!    replay with holes in it. [`Delivery`] splits the two cases: real time
//!    keeps legacy's drop-on-full behaviour, and a historical run paces the
//!    publisher to its subscribers instead. This
//!    is the one place the adapter deliberately behaves differently from legacy
//!    on the same graph; [`Delivery::Lossy`] restores legacy's behaviour in both
//!    modes.
//!
//! One smaller reduction: the server's `CONNECTION_OUTBOUND_CAPACITY` /
//! `SUBSCRIBE_MPSC_CAPACITY` constants stay crate-private here, as in legacy.

mod codec;
mod read;
mod server;
mod write;

pub use codec::{CONTROL_TOPIC, CodecKind, ControlMessage, Envelope, WIRE_PROTOCOL_VERSION};
pub use read::web_sub;
pub use server::{Delivery, WebServer, WebServerBuilder};
pub use write::{WebBurstSinkOps, WebSinkOps};

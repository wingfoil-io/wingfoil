//! web adapter — bidirectional streaming between a wingfoil-next graph and one
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
//! Following the [`lines`](crate::adapters::lines) / [`stats`](crate::stats)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude). Bring in
//! what you need explicitly:
//!
//! - **Source** — the free builder function [`web_sub`] on a
//!   [`GraphBuilder`](crate::fluent::GraphBuilder), emitting
//!   `Stream<Burst<T>>`.
//! - **Sinks** — the [`WebSinkOps`] extension trait on any `Stream<T>` whose
//!   value serializes (one scalar payload per frame), and
//!   [`WebBurstSinkOps`] on `Stream<Burst<T>>` (the whole same-instant group as
//!   one atomic array frame), enabled with
//!   `use wingfoil_next::adapters::web::{WebSinkOps, WebBurstSinkOps};`.
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
//! # Publishing
//!
//! ```ignore
//! use std::time::Duration;
//! use wingfoil_next::{RunFor, RunMode};
//! use wingfoil_next::adapters::web::{WebServer, WebSinkOps};
//! use wingfoil_next::prelude::*;
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
//! use wingfoil_next::adapters::web::{WebServer, web_sub};
//! use wingfoil_next::prelude::*;
//!
//! let server = WebServer::bind("127.0.0.1:0").start()?;
//! let g = GraphBuilder::new();
//! let clicks = web_sub::<u32>(&g, &server, "ui_events")?;
//! let _sink = clicks.collapse().print();
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
//! [`RunMode::HistoricalFrom`](wingfoil_next::RunMode::HistoricalFrom):
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
//! Streaming clients never back-pressure the graph: a client that cannot keep up
//! drops frames (the broadcast buffer is lossy). For a faithful, loss-free
//! replay, keep the graph from outrunning the client — e.g. a genuinely
//! compute-bound historical run.
//!
//! ## Runtime requirement (a `block_on` footgun)
//!
//! `web_pub` drives its encode + broadcast off the graph thread with
//! [`consume_async`](crate::async_source::consume_async), which uses
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
//!    returns [`Result`](anyhow::Result).** Every next source wires on the
//!    builder; the `Result` covers the graph's tokio-runtime creation.
//!    Historical mode is *not* rejected at wiring (unlike the live `_sub`
//!    sources of register B2): `web_sub` is finite under historical replay —
//!    it yields an immediately-ending empty stream, exactly as legacy does.
//! 2. **The sink is a trait only.** Legacy exposed both a free `web_pub`
//!    function and a `WebPubOperators` trait; next folds the entry point into
//!    the [`WebSinkOps`] trait, per the sink-as-trait convention shared with
//!    [`lines`](crate::adapters::lines) / [`csv`](crate::adapters::csv) /
//!    [`kafka`](crate::adapters::kafka), and it returns the sink `Stream<()>`
//!    rather than an `Rc<dyn Node>`.
//! 3. **A burst overload is added.** Legacy could only publish an atomic
//!    same-instant array by mapping `Burst<T>` to `Vec<T>` by hand
//!    (`Burst`/`TinyVec` is not `Serialize`, so it cannot be a second impl of
//!    the same trait); next adds [`WebBurstSinkOps::web_pub_bursts`], which does
//!    that conversion internally and produces byte-identical frames. The manual
//!    `.map(|b| b.to_vec()).web_pub(..)` route still works.
//! 4. **`Complete` is emitted from the sink's teardown**, not from the consumer
//!    noticing its source ended. next's [`consume_async`](crate::async_source::consume_async)
//!    hands back a `flush` teardown; `web_pub` chains its own `finally` that
//!    flushes every queued frame, joins the consumer, and *then* broadcasts
//!    `Complete { topic }` — so the marker still arrives strictly after the last
//!    data frame, on the same broadcast channel, for both a finite `RunFor` and
//!    the end of a historical replay.
//! 5. **The envelope is encoded off the graph thread**, inside the
//!    `consume_async` consumer, as legacy did — the graph thread only clones
//!    the `(time, value)` pair into the sink channel. `tokio::sync::broadcast`
//!    takes an internal lock on `send`, so it must not be touched from a cycle
//!    (the no-locks-on-the-graph-path invariant).
//!
//! One smaller reduction: the server's `PUBLISH_BROADCAST_CAPACITY` /
//! `CONNECTION_OUTBOUND_CAPACITY` / `SUBSCRIBE_MPSC_CAPACITY` constants stay
//! crate-private here, as in legacy.

mod codec;
mod read;
mod server;
mod write;

pub use codec::{CONTROL_TOPIC, CodecKind, ControlMessage, Envelope, WIRE_PROTOCOL_VERSION};
pub use read::web_sub;
pub use server::{WebServer, WebServerBuilder};
pub use write::{WebBurstSinkOps, WebSinkOps};

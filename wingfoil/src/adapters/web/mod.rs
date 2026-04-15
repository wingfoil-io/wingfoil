//! web adapter — bidirectional streaming between a wingfoil graph and
//! one or more browsers over WebSocket.
//!
//! The [`WebServer`] hosts an HTTP + WebSocket listener in its own
//! dedicated tokio runtime (same pattern as the Prometheus exporter).
//! Graph nodes register topics on the server:
//!
//! - [`web_pub`] — publishes a stream's values to every client that
//!   subscribes to the topic.
//! - [`web_sub`] — exposes frames sent by the browser on a topic as a
//!   wingfoil [`Stream`](crate::Stream).
//!
//! Binary frames use [`bincode`](https://docs.rs/bincode) by default;
//! pass [`CodecKind::Json`] for a human-readable mode useful for
//! debugging in the browser devtools. The shared [`Envelope`] type is
//! declared in the [`wingfoil-wire-types`](wingfoil_wire_types) crate
//! so the server binary and the `wingfoil-wasm` browser client can
//! share a single source of truth.
//!
//! # Publishing
//!
//! ```ignore
//! use wingfoil::*;
//! use wingfoil::adapters::web::*;
//! use std::time::Duration;
//!
//! let server = WebServer::bind("127.0.0.1:0").start().unwrap();
//! let port = server.port();
//! println!("open ws://127.0.0.1:{port}/ws");
//!
//! ticker(Duration::from_millis(10))
//!     .count()
//!     .web_pub(&server, "tick")
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```
//!
//! # Subscribing (browser → graph)
//!
//! ```ignore
//! use wingfoil::*;
//! use wingfoil::adapters::web::*;
//!
//! let server = WebServer::bind("127.0.0.1:0").start().unwrap();
//! let clicks: std::rc::Rc<dyn Stream<Burst<u32>>> = web_sub(&server, "ui_events");
//! clicks.collapse().print().run(RunMode::RealTime, RunFor::Forever).unwrap();
//! ```
//!
//! # Serving a static UI bundle
//!
//! ```ignore
//! let server = WebServer::bind("127.0.0.1:3000")
//!     .serve_static("./wingfoil-js/dist")
//!     .start()
//!     .unwrap();
//! ```
//!
//! # Historical mode
//!
//! The server exposes [`WebServerBuilder::start_historical`] for use in
//! `RunMode::HistoricalFrom` runs. No TCP port is bound and all
//! publishes / subscribes become no-ops, so the same graph can run
//! under both real-time and historical modes without modification.

mod codec;
mod read;
mod server;
mod write;

#[cfg(all(test, feature = "web-integration-test"))]
mod integration_tests;

pub use codec::{
    CONTROL_TOPIC, Codec, CodecKind, ControlMessage, Envelope, WIRE_PROTOCOL_VERSION, wire_version,
};
pub use read::web_sub;
pub use server::{WebServer, WebServerBuilder, addr};
pub use write::{WebPubOperators, web_pub};

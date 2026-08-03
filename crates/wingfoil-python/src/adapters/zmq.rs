//! Python bindings for the wingfoil **ZeroMQ** adapter
//! ([`wingfoil::adapters::zmq`]).
//!
//! Four graph entry points, all `#[pyadapter]`-generated:
//!
//! | Python                        | Rust            | shape |
//! |-------------------------------|-----------------|-------|
//! | `zmq_sub(graph, …)`           | [`zmq_sub`]     | `SUB` socket → `(data, status)` |
//! | `zmq_sub_etcd(graph, …)`      | [`zmq_sub`]     | the same, address looked up in etcd |
//! | `zmq_pub(stream, …)`          | [`ZeroMqPub`]   | `PUB` socket sink |
//! | `zmq_pub_etcd(stream, …)`     | [`ZeroMqPub`]   | the same, address registered in etcd |
//!
//! The `_etcd` pair is compiled only when the binding's `etcd` feature is also
//! on, matching the engine's own gating of [`EtcdRegistry`].
//!
//! # The tuple return
//!
//! [`zmq_sub`] hands back **two** streams — the data and a connection-status
//! stream — so this is the binding that made `#[pyadapter]` accept a tuple
//! return, mirroring what `#[pygraph]` already did. In Python:
//!
//! ```python
//! data, status = wingfoil.zmq_sub(g, "tcp://localhost:5556")
//! ```
//!
//! Both are ordinary `Stream`s on the same graph, so either can be composed
//! onward. `status` ticks only on a **transition**, carrying `"connected"` or
//! `"disconnected"` — a string rather than a `#[pyclass]` enum, the convention
//! postgres set.
//!
//! # The payload
//!
//! Payloads cross as **`bytes`**. The Rust source is generic over a
//! `DeserializeOwned` record; the binding instantiates it at `Vec<u8>`, exactly
//! as the legacy binding did, so a Python peer stays wire-compatible with a
//! legacy/next Rust peer that publishes the same type.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **The run mode is an argument.** `zmq_sub` rejects a historical run at
//!    wiring (a live subscriber has no timeline to replay), and a Python `Graph`
//!    does not know its mode until `run()` — so `realtime` is explicit and must
//!    match the eventual `graph.run(realtime=…)`. `realtime=False` raises here.
//! 2. **Status is a string, not an object.** Legacy erased `ZmqStatus` through
//!    its `Debug`; here it is an explicit `"connected"` / `"disconnected"`.
//! 3. **`bind_address` is exposed** on the sinks (legacy bound `0.0.0.0`
//!    implicitly), so a publisher can bind a specific interface.

use anyhow::Result;
use wingfoil::adapters::zmq::{ZeroMqPub, ZmqStatus, zmq_sub as zmq_subscribe};
use wingfoil::prelude::{Burst, GraphBuilder, Stream};

use crate::adapters::common::run_mode;
use crate::{PyElement, pyadapter};

/// A connection-status transition erases to `"connected"` / `"disconnected"`.
impl From<ZmqStatus> for PyElement {
    fn from(status: ZmqStatus) -> Self {
        let name = match status {
            ZmqStatus::Connected => "connected",
            ZmqStatus::Disconnected => "disconnected",
        };
        PyElement::from(name)
    }
}

/// Subscribe to a ZeroMQ publisher at `address`.
///
/// Returns a **`(data, status)` tuple**:
///
/// - `data` ticks with a `list` of `bytes` — the messages received between
///   graph cycles, losslessly grouped;
/// - `status` ticks on each connection transition, carrying `"connected"` or
///   `"disconnected"` (transitions only, not every cycle).
///
/// ```python
/// data, status = zmq_sub(g, "tcp://localhost:5556")
/// ```
///
/// `realtime` declares the run mode this source is wired for and must match the
/// eventual `graph.run(realtime=…)`. A subscriber is a live, never-closing
/// source with no historical timeline, so `realtime=False` raises here.
///
/// The socket connects on a background thread at graph start, so an unreachable
/// publisher aborts the run rather than raising at wiring.
#[pyadapter(name = zmq_sub, source)]
#[pyo3(signature = (address, realtime = true))]
fn sub(
    g: &GraphBuilder,
    address: String,
    realtime: bool,
) -> Result<(Stream<Burst<Vec<u8>>>, Stream<ZmqStatus>)> {
    zmq_subscribe::<Vec<u8>>(g, run_mode(realtime), address)
}

/// Publish this stream on a ZeroMQ `PUB` socket bound to `port`.
///
/// Each tick's value must be `bytes`. Returns a terminal stream whose value is
/// `None`.
///
/// `bind_address` is the interface to bind (`"0.0.0.0"` by default, which is
/// what the legacy binding always used).
#[pyadapter(name = zmq_pub)]
#[pyo3(signature = (port, bind_address = "0.0.0.0".to_string()))]
fn publish(stream: &Stream<Vec<u8>>, port: u16, bind_address: String) -> Result<Stream<()>> {
    Ok(stream.zmq_pub_on(&bind_address, port, ()))
}

/// Subscribe to a named ZeroMQ publisher, resolving its address through etcd.
///
/// The lookup happens at **wiring**, so the publisher must already be
/// registered; an absent key or an unreachable etcd raises here. Returns the
/// same `(data, status)` tuple as [`zmq_sub`].
#[cfg(feature = "etcd")]
#[pyadapter(name = zmq_sub_etcd, source)]
#[pyo3(signature = (name, endpoint, realtime = true))]
fn sub_etcd(
    g: &GraphBuilder,
    name: String,
    endpoint: String,
    realtime: bool,
) -> Result<(Stream<Burst<Vec<u8>>>, Stream<ZmqStatus>)> {
    use wingfoil::adapters::zmq::EtcdRegistry;
    zmq_subscribe::<Vec<u8>>(
        g,
        run_mode(realtime),
        (name.as_str(), EtcdRegistry::new(endpoint)),
    )
}

/// Publish on a `PUB` socket and register the address in etcd under `name`, so
/// [`zmq_sub_etcd`] peers can discover it.
///
/// The registration is held under a lease and revoked at teardown.
#[cfg(feature = "etcd")]
#[pyadapter(name = zmq_pub_etcd)]
#[pyo3(signature = (port, name, endpoint, bind_address = "0.0.0.0".to_string()))]
fn publish_etcd(
    stream: &Stream<Vec<u8>>,
    port: u16,
    name: String,
    endpoint: String,
    bind_address: String,
) -> Result<Stream<()>> {
    use wingfoil::adapters::zmq::EtcdRegistry;
    Ok(stream.zmq_pub_on(
        &bind_address,
        port,
        (name.as_str(), EtcdRegistry::new(endpoint)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_erases_to_a_string() {
        for (status, expected) in [
            (ZmqStatus::Connected, "connected"),
            (ZmqStatus::Disconnected, "disconnected"),
        ] {
            let element = PyElement::from(status);
            assert_eq!(expected, String::try_from(&element).unwrap());
        }
    }
}

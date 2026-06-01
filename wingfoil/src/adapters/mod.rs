//! A library of input and output adapters

/// Interpret a byte slice as a UTF-8 string.
///
/// Shared `value_str`/`key_str` accessor for message-payload adapters
/// (kafka, fluvio, etcd) so the `from_utf8` boilerplate lives in one place.
#[cfg(any(feature = "kafka", feature = "fluvio", feature = "etcd"))]
pub(crate) fn bytes_str(bytes: &[u8]) -> Result<&str, std::str::Utf8Error> {
    std::str::from_utf8(bytes)
}

/// Wrap a single-item stream into a `Burst`-of-one stream.
///
/// Shared helper for the `*_pub` fluent operators, whose single-value impls all
/// delegate to their `Burst`-based counterpart after wrapping each item.
#[cfg(any(feature = "kafka", feature = "fluvio", feature = "etcd"))]
pub(crate) fn single_to_burst<T: crate::types::Element>(
    stream: &std::rc::Rc<dyn crate::types::Stream<T>>,
) -> std::rc::Rc<dyn crate::types::Stream<crate::types::Burst<T>>> {
    use crate::nodes::StreamOperators;
    stream.map(|item| crate::burst![item])
}

#[cfg(feature = "kdb")]
pub mod cache;
#[cfg(feature = "csv")]
pub mod csv;
#[cfg(feature = "etcd")]
pub mod etcd;
#[cfg(feature = "fix")]
#[doc(hidden)]
pub mod fix;
#[cfg(feature = "fluvio")]
pub mod fluvio;
#[cfg(feature = "iceoryx2")]
#[doc(hidden)]
pub mod iceoryx2;
#[cfg(feature = "kafka")]
pub mod kafka;
#[cfg(feature = "kdb")]
pub mod kdb;
#[cfg(feature = "otlp")]
pub mod otlp;
#[cfg(feature = "prometheus")]
pub mod prometheus;
#[cfg(feature = "web")]
pub mod web;
#[cfg(feature = "zmq")]
pub mod zmq;

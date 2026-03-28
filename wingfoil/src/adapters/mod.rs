//! A library of input and output adapters

#[cfg(feature = "kdb")]
pub mod cache;
#[cfg(feature = "csv")]
pub mod csv_streams;
#[cfg(feature = "grafana")]
pub mod grafana;
pub mod iterator_stream;
#[cfg(feature = "kdb")]
pub mod kdb;
#[cfg(feature = "zmq-beta")]
#[doc(hidden)]
pub mod zmq;

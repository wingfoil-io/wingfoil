//! A library of input and output adapters

#[cfg(feature = "kdb")]
pub mod cache;
#[cfg(feature = "csv")]
pub mod csv;
#[cfg(feature = "etcd")]
pub mod etcd;
#[cfg(feature = "iceoryx2-beta")]
#[doc(hidden)]
pub mod iceoryx2;
pub mod iterator_stream;
#[cfg(feature = "kdb")]
pub mod kdb;
#[cfg(feature = "otlp")]
pub mod otlp;
#[cfg(feature = "prometheus")]
pub mod prometheus;
#[cfg(feature = "zmq-beta")]
#[doc(hidden)]
pub mod zmq;

//! A library of input and output adapters

pub mod aeron;
#[cfg(feature = "kdb")]
pub mod cache;
#[cfg(feature = "csv")]
pub mod csv_streams;
#[cfg(feature = "etcd")]
pub mod etcd;
pub mod iterator_stream;
#[cfg(feature = "kdb")]
pub mod kdb;
#[cfg(feature = "zmq-beta")]
#[doc(hidden)]
pub mod zmq;

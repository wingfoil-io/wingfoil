//! A library of input and output adapters

#[cfg(feature = "kdb")]
pub mod cache;
#[cfg(feature = "csv")]
pub mod csv;
#[cfg(feature = "etcd")]
pub mod etcd;
#[cfg(feature = "kdb")]
pub mod kdb;
#[cfg(feature = "zmq-beta")]
#[doc(hidden)]
pub mod zmq;

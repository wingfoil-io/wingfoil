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
#[cfg(feature = "kdb")]
pub mod kdb;
#[cfg(feature = "zmq-beta")]
#[doc(hidden)]
pub mod zmq;

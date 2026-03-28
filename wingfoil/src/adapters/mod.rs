//! A library of input and output adapters

#[cfg(feature = "kdb")]
pub mod cache;
#[cfg(feature = "csv")]
pub mod csv_streams;
#[cfg(feature = "fix")]
#[doc(hidden)]
pub mod fix;
pub mod iterator_stream;
#[cfg(feature = "kdb")]
pub mod kdb;
#[cfg(feature = "websockets")]
#[doc(hidden)]
pub mod socket;
#[cfg(feature = "zmq-beta")]
#[doc(hidden)]
pub mod zmq;

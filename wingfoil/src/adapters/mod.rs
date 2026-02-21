//! A library of input and output adapters

#[cfg(feature = "csv")]
pub mod csv_streams;
pub mod iterator_stream;
#[cfg(feature = "kdb")]
pub mod kdb;
#[cfg(feature = "websockets")]
#[doc(hidden)]
pub mod socket;

//! A library of input and output adapters

pub mod aeron;
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
#[cfg(feature = "iceoryx2-beta")]
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

//! etcd adapter — streaming reads (watch) and writes (put) for etcd key-value store.
//!
//! # Example
//! ```ignore
//! use wingfoil::adapters::etcd::*;
//! use wingfoil::*;
//!
//! let conn = EtcdConnection::new("http://localhost:2379");
//!
//! etcd_sub(conn, "/config/")
//!     .collapse()
//!     .for_each(|event, _| println!("{} = {:?}", event.entry.key, event.entry.value_str()))
//!     .run(RunMode::RealTime, RunFor::Forever)
//!     .unwrap();
//! ```

mod read;
mod write;

#[cfg(all(test, feature = "etcd-integration-test"))]
mod integration_tests;

pub use read::*;
pub use write::*;

/// Connection configuration for etcd.
#[derive(Debug, Clone)]
pub struct EtcdConnection {
    /// etcd endpoints, e.g. `["http://localhost:2379"]`.
    pub endpoints: Vec<String>,
}

impl EtcdConnection {
    /// Create a connection config with a single endpoint.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoints: vec![endpoint.into()],
        }
    }

    /// Create a connection config with multiple endpoints (for an etcd cluster).
    pub fn with_endpoints(endpoints: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            endpoints: endpoints.into_iter().map(Into::into).collect(),
        }
    }
}

/// A single key-value pair from etcd.
#[derive(Debug, Clone, Default)]
pub struct EtcdEntry {
    pub key: String,
    pub value: Vec<u8>,
}

impl EtcdEntry {
    /// Interpret the value as a UTF-8 string.
    pub fn value_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.value)
    }
}

/// The type of change represented by an [`EtcdEvent`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum EtcdEventKind {
    /// A key was created or updated.
    #[default]
    Put,
    /// A key was deleted.
    ///
    /// The associated [`EtcdEvent::entry`] carries the deleted key but an **empty value**
    /// (`value == vec![]`). Use [`EtcdEventKind::Put`] events if you need the value.
    Delete,
}

/// An event from an etcd watch stream.
///
/// Snapshot events (from the initial GET) are always [`EtcdEventKind::Put`].
/// Subsequent watch events reflect the actual change type from etcd.
#[derive(Debug, Clone, Default)]
pub struct EtcdEvent {
    pub kind: EtcdEventKind,
    pub entry: EtcdEntry,
    /// The etcd cluster revision at which this event was observed.
    pub revision: i64,
}

//! KDB+ database adapter for reading and writing data to q/kdb+ instances.
//!
//! This module provides async connectivity to KDB+ databases using the `kdbplus` crate,
//! allowing query results to be streamed into wingfoil graphs.
//!
//! # Example
//!
//! ```ignore
//! use wingfoil::adapters::kdb::*;
//! use wingfoil::*;
//!
//! #[derive(Debug, Clone, Default)]
//! struct Trade {
//!     sym: Sym,
//!     price: f64,
//!     size: i64,
//! }
//!
//! impl KdbDeserialize for Trade {
//!     fn from_kdb_row(row: Row<'_>, _columns: &[String], interner: &mut SymbolInterner) -> Result<(NanoTime, Self), KdbError> {
//!         let time = row.get_timestamp(1)?; // col 0: date, col 1: time
//!         Ok((time, Trade {
//!             sym: row.get_sym(2, interner)?,
//!             price: row.get(3)?.get_float()?,
//!             size: row.get(4)?.get_long()?,
//!         }))
//!     }
//! }
//!
//! let conn = KdbConnection::new("localhost", 5000)
//!     .with_credentials("user", "pass");
//!
//! // Read with time-sliced chunking, one slice per hour
//! kdb_read::<Trade, _>(
//!     conn,
//!     std::time::Duration::from_secs(3600),
//!     |(t0, t1), date, _| {
//!         format!(
//!             "select from trades where date=2000.01.01+{}, \
//!              time >= (`timestamp$){}j, time < (`timestamp$){}j",
//!             date, t0.to_kdb_timestamp(), t1.to_kdb_timestamp()
//!         )
//!     },
//! )
//!     .collapse()
//!     .map(|trade| trade.price)
//!     .print()
//!     .run(
//!         RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(0)),
//!         RunFor::Duration(std::time::Duration::from_secs(86400)),
//!     )
//!     .unwrap();
//! ```

mod read;
mod read_cached;
mod write;

#[cfg(all(test, feature = "kdb-integration-test"))]
mod cache_integration_tests;
#[cfg(all(test, feature = "kdb-integration-test"))]
mod integration_tests;

pub use crate::adapters::cache::CacheConfig;
pub use read::*;
pub use read_cached::*;
pub use write::*;

/// Re-export kdbplus error type for convenience.
pub use kdb_plus_fixed::ipc::error::Error as KdbError;

/// Re-export K type for building custom serialization.
pub use kdb_plus_fixed::ipc::K;

use std::collections::HashSet;
use std::sync::Arc;

/// An interned symbol string, backed by `Arc<str>` for cheap cloning and deduplication.
///
/// Use with [`SymbolInterner`] to ensure repeated symbol values (e.g. `"AAPL"`, `"GOOG"`)
/// share a single heap allocation.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct Sym(Arc<str>);

impl std::fmt::Debug for Sym {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::fmt::Display for Sym {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Default for Sym {
    fn default() -> Self {
        Sym(Arc::from(""))
    }
}

impl serde::Serialize for Sym {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Sym {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(Sym(Arc::from(s.as_str())))
    }
}

/// Deduplicates symbol strings so repeated values share a single `Arc<str>` allocation.
///
/// Created once per read call and passed to `from_kdb_row` / `Row::get_sym`.
#[derive(Default)]
pub struct SymbolInterner {
    set: HashSet<Arc<str>>,
}

impl SymbolInterner {
    /// Intern a string, returning a [`Sym`] that shares storage with prior equal values.
    pub fn intern(&mut self, s: &str) -> Sym {
        if let Some(existing) = self.set.get(s) {
            Sym(Arc::clone(existing))
        } else {
            let arc: Arc<str> = Arc::from(s);
            self.set.insert(Arc::clone(&arc));
            Sym(arc)
        }
    }
}

/// KDB connection configuration.
#[derive(Debug, Clone)]
pub struct KdbConnection {
    /// Host address of the KDB server.
    pub host: String,
    /// Port number of the KDB server.
    pub port: u16,
    /// Optional authentication credentials.
    pub credentials: Option<KdbCredentials>,
}

/// Authentication credentials for KDB connection.
#[derive(Debug, Clone)]
pub struct KdbCredentials {
    /// Username for authentication.
    pub username: String,
    /// Password for authentication.
    pub password: String,
}

impl KdbConnection {
    /// Create a new KDB connection configuration.
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            credentials: None,
        }
    }

    /// Add authentication credentials to the connection.
    pub fn with_credentials(
        mut self,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        self.credentials = Some(KdbCredentials {
            username: username.into(),
            password: password.into(),
        });
        self
    }

    /// Build the credentials string for KDB connection.
    /// Format: "username:password" or empty string if no credentials.
    pub fn credentials_string(&self) -> String {
        match &self.credentials {
            Some(creds) => format!("{}:{}", creds.username, creds.password),
            None => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kdb_connection_new() {
        let conn = KdbConnection::new("localhost", 5000);
        assert_eq!(conn.host, "localhost");
        assert_eq!(conn.port, 5000);
        assert!(conn.credentials.is_none());
    }

    #[test]
    fn test_kdb_connection_with_credentials() {
        let conn = KdbConnection::new("localhost", 5000).with_credentials("user", "pass");
        assert!(conn.credentials.is_some());
        let creds = conn.credentials.unwrap();
        assert_eq!(creds.username, "user");
        assert_eq!(creds.password, "pass");
    }

    #[test]
    fn test_credentials_string() {
        let conn = KdbConnection::new("localhost", 5000);
        assert_eq!(conn.credentials_string(), "");

        let conn_with_creds =
            KdbConnection::new("localhost", 5000).with_credentials("user", "pass");
        assert_eq!(conn_with_creds.credentials_string(), "user:pass");
    }
}

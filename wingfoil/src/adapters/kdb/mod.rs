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
//!     fn from_kdb_row(row: Row<'_>, _columns: &[String], interner: &mut SymbolInterner) -> Result<Self, KdbError> {
//!         Ok(Trade {
//!             sym: row.get_sym(1, interner)?,
//!             price: row.get(2)?.get_float()?,
//!             size: row.get(3)?.get_long()?,
//!         })
//!     }
//! }
//!
//! let conn = KdbConnection::new("localhost", 5000)
//!     .with_credentials("user", "pass");
//!
//! // Read with time-based chunking for memory-bounded streaming
//! kdb_read(
//!     conn,
//!     "select from trades where date=.z.d",
//!     "time",                  // time column for chunking
//!     10000,                   // rows_per_chunk (controls memory usage)
//! )
//!     .map(|trades| trades.first().map(|t| t.price).unwrap_or(0.0))
//!     .print()
//!     .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
//!     .unwrap();
//! ```

mod read;
mod write;

#[cfg(all(test, feature = "kdb-integration-test"))]
mod integration_tests;

pub use read::*;
pub use write::*;

/// Re-export kdbplus error type for convenience.
pub use kdbplus::ipc::error::Error as KdbError;

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

/// Deduplicates symbol strings so repeated values share a single `Arc<str>` allocation.
///
/// Created once per `kdb_read` call and passed to `from_kdb_row` / `Row::get_sym`.
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

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
//!     sym: String,
//!     time: i64,
//!     price: f64,
//!     size: i64,
//! }
//!
//! impl KdbDeserialize for Trade {
//!     fn from_kdb_row(row: &K, columns: &[String]) -> Result<Self, KdbError> {
//!         // Extract fields from row - use row.as_vec() to get column values
//!         Ok(Trade { ... })
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
//!     0,                       // start_time (KDB epoch: 2000-01-01)
//!     86400000000000,          // end_time (24 hours later)
//!     10000,                   // rows_per_chunk (controls memory usage)
//!     |t: &Trade| NanoTime::from_kdb_timestamp(t.time)
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

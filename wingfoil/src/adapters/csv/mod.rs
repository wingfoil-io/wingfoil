//! CSV adapter — read and write comma-separated values files.
//!
//! Provides one read function and a fluent write operator:
//!
//! - [`csv_read`] — producer that emits each tick's records as a [`Burst<T>`]
//! - [`CsvOperators::csv_write`] — consumer that writes a `Burst<T>` stream to a CSV file
//!
//! Record types must implement [`serde::Serialize`] and [`serde::de::DeserializeOwned`].
//!
//! # Reading
//!
//! ```ignore
//! use wingfoil::adapters::csv::*;
//! use wingfoil::*;
//!
//! #[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
//! struct Row { timestamp: u64, value: f64 }
//!
//! // Multiple rows with the same timestamp arrive as a Burst.
//! // Use .collapse() when the source is strictly ascending.
//! csv_read("data.csv", |r: &Row| NanoTime::new(r.timestamp), true)
//!     .collapse()
//!     .for_each(|row, _| println!("{:?}", row))
//!     .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
//!     .unwrap();
//! ```
//!
//! # Writing
//!
//! ```ignore
//! use wingfoil::adapters::csv::*;
//! use wingfoil::*;
//!
//! csv_read("input.csv", get_time, true)
//!     .csv_write("output.csv")
//!     .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
//!     .unwrap();
//! ```

mod read;
mod write;

pub use read::*;
pub use write::*;

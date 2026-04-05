use crate::burst;
use derive_new::new;
use serde::{Serialize, de::DeserializeOwned};
use serde_aux::serde_introspection::serde_introspect;
use std::fs::File;
use std::rc::Rc;

use crate::nodes::StreamOperators;
use crate::types::*;

/// Writes a [`Burst<T>`] stream to a CSV file, one row per element per tick.
/// Used by [`CsvOperators::csv_write`].
#[derive(new)]
pub struct CsvWriterNode<T: Element> {
    upstream: Rc<dyn Stream<Burst<T>>>,
    writer: csv::Writer<File>,
    #[new(default)]
    headers_written: bool,
}

#[node(active = [upstream])]
impl<T: Element + Serialize + DeserializeOwned + 'static> MutableNode for CsvWriterNode<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        if !self.headers_written {
            write_header::<T>(&mut self.writer);
            self.headers_written = true;
        }
        for rec in self.upstream.peek_value() {
            self.writer
                .serialize((state.time(), rec))
                .map_err(|e| anyhow::anyhow!("Failed to serialize CSV record: {e}"))?;
        }
        Ok(false)
    }
}

fn write_header<T: Serialize + DeserializeOwned + 'static>(writer: &mut csv::Writer<File>) {
    let fields = serde_introspect::<T>();
    if !fields.is_empty() {
        writer.write_field("time").unwrap();
        writer.write_record(fields).unwrap();
    }
}

/// Trait to add csv write operators to streams.
pub trait CsvOperators<T: Element> {
    /// Writes each element of the burst to a CSV file, one row per element per tick.
    #[must_use]
    fn csv_write(self: &Rc<Self>, path: &str) -> Rc<dyn Node>;
}

impl<T: Element + Serialize + DeserializeOwned + 'static> CsvOperators<T> for dyn Stream<Burst<T>> {
    fn csv_write(self: &Rc<Self>, path: &str) -> Rc<dyn Node> {
        let writer = csv::WriterBuilder::new()
            .has_headers(false)
            .from_path(path)
            .unwrap();
        CsvWriterNode::new(self.clone(), writer).into_node()
    }
}

impl<T: Element + Serialize + DeserializeOwned + 'static> CsvOperators<T> for dyn Stream<T> {
    fn csv_write(self: &Rc<Self>, path: &str) -> Rc<dyn Node> {
        self.map(|v| burst![v]).csv_write(path)
    }
}

#[cfg(test)]
mod tests {
    use crate::adapters::csv::*;
    use crate::graph::*;
    use crate::nodes::*;
    use crate::types::NanoTime;
    use std::time::Duration;

    type Record = (NanoTime, u32);

    #[test]
    pub fn csv_simple_works() {
        let run_to = RunFor::Duration(Duration::from_nanos(1));
        let run_mode = RunMode::HistoricalFrom(NanoTime::ZERO);
        let get_time = |rec: &Record| rec.0 as NanoTime;
        csv_read("src/adapters/csv/test_data/simple.csv", get_time, false)
            .csv_write("src/adapters/csv/test_data/simple_out.csv")
            .run(run_mode, run_to)
            .unwrap();
    }

    #[test]
    pub fn csv_multi_timestamp_works() {
        let run_to = RunFor::Forever;
        let run_mode = RunMode::HistoricalFrom(NanoTime::ZERO);
        let get_time = |rec: &Record| rec.0 as NanoTime;
        csv_read("src/adapters/csv/test_data/example.csv", get_time, false)
            .csv_write("src/adapters/csv/test_data/example_out.csv")
            .run(run_mode, run_to)
            .unwrap();
    }
}

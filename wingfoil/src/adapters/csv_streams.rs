//! Reading and writing to csv files.

use derive_new::new;

use serde::{Serialize, de::DeserializeOwned};
use serde_aux::serde_introspection::serde_introspect;
use std::fs::File;
use std::rc::Rc;

use crate::adapters::iterator_stream::{IteratorStream, SimpleIteratorStream};
use crate::queue::ValueAt;
use crate::types::*;

fn csv_iterator<T>(
    path: &str,
    get_time_func: impl Fn(&T) -> NanoTime + 'static,
    has_headers: bool,
) -> anyhow::Result<Box<dyn Iterator<Item = ValueAt<T>>>>
where
    T: Element + DeserializeOwned + 'static,
{
    let file = File::open(path)?;
    let data = csv::ReaderBuilder::new()
        .has_headers(has_headers)
        .from_reader(file)
        .into_deserialize();
    Ok(Box::new(data.filter_map(
        move |record: Result<T, csv::Error>| match record {
            Ok(rec) => {
                let t = get_time_func(&rec);
                Some(ValueAt {
                    value: rec,
                    time: t,
                })
            }
            Err(e) => {
                log::error!("failed to parse CSV record: {e}");
                None
            }
        },
    )))
}

/// Returns a [SimpleIteratorStream] that emits records from a file of
/// comma separated values (csv).  The source iterator must be of strictly
/// ascending time.  For the more general cases where there can be multiple
/// rows with the same timestamp, you can use [csv_read_vec] instead.
pub fn csv_read<T>(
    path: &str,
    get_time_func: impl Fn(&T) -> NanoTime + 'static,
    has_headers: bool,
) -> anyhow::Result<Rc<dyn Stream<T>>>
where
    T: Element + DeserializeOwned + 'static,
{
    let it = csv_iterator(path, get_time_func, has_headers)?;
    Ok(SimpleIteratorStream::new(it).into_stream())
}

/// Returns a [IteratorStream] that emits values from a file of
/// comma separated values (csv).  The source iterator can tick
/// multiple times per cycle.
pub fn csv_read_vec<T>(
    path: &str,
    get_time_func: impl Fn(&T) -> NanoTime + 'static,
    has_headers: bool,
) -> anyhow::Result<Rc<dyn Stream<Vec<T>>>>
where
    T: Element + DeserializeOwned + 'static,
{
    let it = csv_iterator(path, get_time_func, has_headers)?;
    Ok(IteratorStream::new(it).into_stream())
}

/// Used to write records to a file of comma separated values (csv).
/// Used by [write_csv](crate::adapters::csv_streams::CsvOperators::csv_write).
#[derive(new)]
pub struct CsvWriterNode<T> {
    upstream: Rc<dyn Stream<T>>,
    writer: csv::Writer<File>,
    #[new(default)]
    headers_written: bool,
}

impl<T: Serialize + DeserializeOwned + 'static> MutableNode for CsvWriterNode<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        if !self.headers_written {
            write_header::<T>(&mut self.writer)?;
            self.headers_written = true;
        }
        self.writer
            .serialize((state.time(), self.upstream.peek_value()))
            .map_err(|e| anyhow::anyhow!("Failed to serialize CSV record: {e}"))?;
        Ok(false)
    }
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

fn write_header<T: Serialize + DeserializeOwned + 'static>(
    writer: &mut csv::Writer<File>,
) -> anyhow::Result<()> {
    let fields = serde_introspect::<T>();
    if !fields.is_empty() {
        writer
            .write_field("time")
            .map_err(|e| anyhow::anyhow!("failed to write CSV header field: {e}"))?;
        writer
            .write_record(fields)
            .map_err(|e| anyhow::anyhow!("failed to write CSV header record: {e}"))?;
    }
    Ok(())
}

/// Used to write records to a file of comma separated values (csv).
/// Used by [write_csv](crate::adapters::csv_streams::CsvVecOperators::csv_write_vec).
#[derive(new)]
pub struct CsvVecWriterNode<T> {
    upstream: Rc<dyn Stream<Vec<T>>>,
    writer: csv::Writer<File>,
    #[new(default)]
    headers_written: bool,
}

impl<T: Element + Serialize + DeserializeOwned + 'static> MutableNode for CsvVecWriterNode<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        if !self.headers_written {
            write_header::<T>(&mut self.writer)?;
            self.headers_written = true;
        }
        for rec in self.upstream.peek_value() {
            self.writer
                .serialize((state.time(), rec))
                .map_err(|e| anyhow::anyhow!("Failed to serialize CSV record: {e}"))?;
        }
        Ok(false)
    }
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

/// Trait to add csv write operators to streams.
pub trait CsvOperators<T: Element> {
    /// writes stream to csv file
    fn csv_write(self: &Rc<Self>, path: &str) -> anyhow::Result<Rc<dyn Node>>;
}

impl<T: Element + Serialize + DeserializeOwned + 'static> CsvOperators<T> for dyn Stream<T> {
    fn csv_write(self: &Rc<Self>, path: &str) -> anyhow::Result<Rc<dyn Node>> {
        let writer = csv::WriterBuilder::new()
            .has_headers(false)
            .from_path(path)?;
        Ok(CsvWriterNode::new(self.clone(), writer).into_node())
    }
}
pub trait CsvVecOperators<T: Element> {
    /// writes stream of Vec to csv file
    fn csv_write_vec(self: &Rc<Self>, path: &str) -> anyhow::Result<Rc<dyn Node>>;
}

impl<T: Element + Serialize + DeserializeOwned + 'static> CsvVecOperators<T>
    for dyn Stream<Vec<T>>
{
    fn csv_write_vec(self: &Rc<Self>, path: &str) -> anyhow::Result<Rc<dyn Node>> {
        let writer = csv::WriterBuilder::new()
            .has_headers(false)
            .from_path(path)?;
        Ok(CsvVecWriterNode::new(self.clone(), writer).into_node())
    }
}

#[cfg(test)]
mod tests {

    use crate::adapters::csv_streams::*;
    use crate::graph::*;
    use crate::nodes::*;
    use std::time::Duration;

    type Record = (NanoTime, u32);

    #[test]
    pub fn csv_simple_works() {
        //env_logger::init();
        let run_to = RunFor::Duration(Duration::from_nanos(1));
        let run_mode = RunMode::HistoricalFrom(NanoTime::ZERO);
        let get_time = |rec: &Record| rec.0 as NanoTime;
        csv_read("test_data/simple.csv", get_time, false)
            .unwrap()
            .csv_write("test_data/simple_out.csv")
            .unwrap()
            .run(run_mode, run_to)
            .unwrap();
    }

    #[test]
    pub fn csv_vec_works() {
        //env_logger::init();
        let run_to = RunFor::Forever;
        let run_mode = RunMode::HistoricalFrom(NanoTime::ZERO);
        let get_time = |rec: &Record| rec.0 as NanoTime;
        csv_read_vec("test_data/example.csv", get_time, false)
            .unwrap()
            .csv_write_vec("test_data/example_out.csv")
            .unwrap()
            .run(run_mode, run_to)
            .unwrap();
    }
}

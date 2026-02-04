//! Reading and writing to csv files.

use derive_new::new;

use serde::{Serialize, de::DeserializeOwned};
use serde_aux::serde_introspection::serde_introspect;
use std::fs::File;
use std::rc::Rc;
use std::fmt::Debug as StdDebug;

use crate::adapters::iterator_stream::{IteratorStream, SimpleIteratorStream};
use crate::queue::ValueAt;
use crate::types::*;

fn csv_iterator<T>(
    path: &str,
    get_time_func: impl Fn(&T) -> NanoTime + 'static,
    has_headers: bool,
) -> Box<dyn Iterator<Item = ValueAt<T>>>
where
    T: StdDebug + Clone + Default + DeserializeOwned + 'static,
{
    let data = csv::ReaderBuilder::new()
        .has_headers(has_headers)
        .from_reader(File::open(path).unwrap())
        .into_deserialize();
    Box::new(data.map(move |record: Result<T, csv::Error>| {
        let rec = record.unwrap();
        let t = get_time_func(&rec);
        ValueAt {
            value: rec,
            time: t,
        }
    }))
}

/// Returns a [SimpleIteratorStream] that emits records from a file of
/// comma separated values (csv).  The source iterator must be of strictly
/// ascending time.  For the more general cases where there can be multiple
/// rows with the same timestamp, you can use [csv_read_vec] instead.
pub fn csv_read<'a, T>(
    path: &str,
    get_time_func: impl Fn(&T) -> NanoTime + 'static,
    has_headers: bool,
) -> Rc<dyn Stream<'a, T> + 'a>
where
    T: StdDebug + Clone + Default + DeserializeOwned + 'a + 'static,
{
    let it = csv_iterator(path, get_time_func, has_headers);
    SimpleIteratorStream::new(it).into_stream()
}

/// Returns a [IteratorStream] that emits values from a file of
/// comma separated values (csv).  The source iterator can tick
/// multiple times per cycle.  
pub fn csv_read_vec<'a, T>(
    path: &str,
    get_time_func: impl Fn(&T) -> NanoTime + 'static,
    has_headers: bool,
) -> Rc<dyn Stream<'a, Vec<T>> + 'a>
where
    T: StdDebug + Clone + Default + DeserializeOwned + 'a + 'static,
{
    let it = csv_iterator(path, get_time_func, has_headers);
    IteratorStream::new(it).into_stream()
}

/// Used to write records to a file of comma separated values (csv).
/// Used by [write_csv](crate::adapters::csv_streams::CsvOperators::csv_write).
#[derive(new)]
pub struct CsvWriterNode<'a, T> {
    upstream: Rc<dyn Stream<'a, T> + 'a>,
    writer: csv::Writer<File>,
    #[new(default)]
    headers_written: bool,
}

impl<'a, T: StdDebug + Clone + Serialize + DeserializeOwned + 'a + 'static> MutableNode<'a> for CsvWriterNode<'a, T> {
    fn cycle(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        if !self.headers_written {
            write_header::<T>(&mut self.writer);
            self.headers_written = true;
        }
        self.writer
            .serialize((state.time(), self.upstream.peek_value()))
            .map_err(|e| anyhow::anyhow!("Failed to serialize CSV record: {e}"))?;
        Ok(false)
    }
    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

fn write_header<T: Serialize + DeserializeOwned + 'static>(writer: &mut csv::Writer<File>) {
    let fields = serde_introspect::<T>();
    if !fields.is_empty() {
        writer.write_field("time").unwrap();
        writer.write_record(fields).unwrap();
    }
}

/// Used to write records to a file of comma separated values (csv).
/// Used by [write_csv](crate::adapters::csv_streams::CsvVecOperators::csv_write_vec).
#[derive(new)]
pub struct CsvVecWriterNode<'a, T> {
    upstream: Rc<dyn Stream<'a, Vec<T>> + 'a>,
    writer: csv::Writer<File>,
    #[new(default)]
    headers_written: bool,
}

impl<'a, T: StdDebug + Clone + Serialize + DeserializeOwned + 'a + 'static> MutableNode<'a> for CsvVecWriterNode<'a, T> {
    fn cycle(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<bool> {
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
    fn upstreams(&self) -> UpStreams<'a> {
        UpStreams::new(vec![self.upstream.clone().as_node()], vec![])
    }
}

/// Trait to add csv write operators to streams.
pub trait CsvOperators<'a, T: StdDebug + Clone + Default + 'a> {
    /// writes stream to csv file
    fn csv_write(self: &Rc<Self>, path: &str) -> Rc<dyn Node<'a> + 'a>;
}

impl<'a, T: StdDebug + Clone + Default + Serialize + DeserializeOwned + 'a + 'static> CsvOperators<'a, T> for dyn Stream<'a, T> + 'a {
    fn csv_write(self: &Rc<Self>, path: &str) -> Rc<dyn Node<'a> + 'a> {
        let writer = csv::WriterBuilder::new()
            .has_headers(false)
            .from_path(path)
            .unwrap();
        CsvWriterNode::new(self.clone(), writer).into_node()
    }
}
pub trait CsvVecOperators<'a, T: StdDebug + Clone + Default + 'a> {
    /// writes stream of Vec to csv file
    fn csv_write_vec(self: &Rc<Self>, path: &str) -> Rc<dyn Node<'a> + 'a>;
}

impl<'a, T: StdDebug + Clone + Default + Serialize + DeserializeOwned + 'a + 'static> CsvVecOperators<'a, T>
    for dyn Stream<'a, Vec<T>> + 'a
{
    fn csv_write_vec(self: &Rc<Self>, path: &str) -> Rc<dyn Node<'a> + 'a> {
        let writer = csv::WriterBuilder::new()
            .has_headers(false)
            .from_path(path)
            .unwrap();
        CsvVecWriterNode::new(self.clone(), writer).into_node()
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
            .csv_write("test_data/simple_out.csv")
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
            .csv_write_vec("test_data/example_out.csv")
            .run(run_mode, run_to)
            .unwrap();
    }
}
use serde::de::DeserializeOwned;
use std::fs::File;
use std::rc::Rc;

use crate::nodes::IteratorStream;
use crate::queue::ValueAt;
use crate::types::*;

fn csv_iterator<T>(
    path: &str,
    get_time_func: impl Fn(&T) -> NanoTime + 'static,
    has_headers: bool,
) -> Box<dyn Iterator<Item = ValueAt<T>>>
where
    T: Element + DeserializeOwned + 'static,
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

/// Returns a stream that emits records from a CSV file as a [`Burst<T>`] per tick.
/// Multiple rows sharing the same timestamp are grouped into a single burst.
/// Use [`.collapse()`](crate::StreamOperators::collapse) when the source is
/// strictly ascending and you need a plain `T` per tick.
#[must_use]
pub fn csv_read<T>(
    path: &str,
    get_time_func: impl Fn(&T) -> NanoTime + 'static,
    has_headers: bool,
) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + DeserializeOwned + 'static,
{
    IteratorStream::new(csv_iterator(path, get_time_func, has_headers)).into_stream()
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::*;
    use crate::nodes::StreamOperators;

    type Record = (NanoTime, u32);

    fn get_time(r: &Record) -> NanoTime {
        r.0
    }

    #[test]
    fn csv_read_emits_all_rows() {
        // simple.csv has 6 rows, each with a unique timestamp
        let stream = csv_read("src/adapters/csv/test_data/simple.csv", get_time, false);
        let collected = stream.collect();
        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        // 6 ticks, each burst has 1 element
        let all: Vec<u32> = collected
            .peek_value()
            .iter()
            .flat_map(|b| b.value.iter().map(|r| r.1))
            .collect();
        assert_eq!(all, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn csv_read_each_row_is_single_burst() {
        let stream = csv_read("src/adapters/csv/test_data/simple.csv", get_time, false);
        let collected = stream.collect();
        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        // all ticks emit a burst of exactly 1
        assert!(collected.peek_value().iter().all(|b| b.value.len() == 1));
    }

    #[test]
    fn csv_read_groups_same_timestamp_into_one_burst() {
        // example.csv has two rows at timestamp 1003
        let stream = csv_read("src/adapters/csv/test_data/example.csv", get_time, false);
        let collected = stream.collect();
        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        // 4 distinct timestamps: 1001, 1002, 1003, 1004
        assert_eq!(collected.peek_value().len(), 4);
        // The burst at timestamp 1003 should have 2 elements
        let burst_1003 = collected
            .peek_value()
            .iter()
            .find(|b| b.time == NanoTime::new(1003));
        assert!(burst_1003.is_some());
        assert_eq!(burst_1003.unwrap().value.len(), 2);
    }
}

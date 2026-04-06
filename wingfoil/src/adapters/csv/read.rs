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
    use crate::nodes::{NodeOperators, StreamOperators};

    type Record = (NanoTime, u32);

    fn get_time(r: &Record) -> NanoTime {
        r.0
    }

    #[test]
    fn csv_read_emits_all_rows() {
        // read_test.csv has 6 data rows + a sentinel row (9999,0) so all 6 emit.
        // IteratorStream needs at least one item after the last real item to trigger emission.
        let stream = csv_read("src/adapters/csv/test_data/read_test.csv", get_time, false);
        let collected = stream.collect();
        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let all: Vec<u32> = collected
            .peek_value()
            .iter()
            .flat_map(|b| b.value.iter().map(|r| r.1))
            .filter(|&v| v != 0) // exclude sentinel
            .collect();
        assert_eq!(all, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn csv_read_each_row_is_single_burst() {
        // read_test.csv: 6 data rows, each unique timestamp → 6 single-element bursts
        let stream = csv_read("src/adapters/csv/test_data/read_test.csv", get_time, false);
        let collected = stream.collect();
        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let data_ticks: Vec<_> = collected
            .peek_value()
            .into_iter()
            .filter(|b| b.value.iter().any(|r| r.1 != 0))
            .collect();
        assert_eq!(data_ticks.len(), 6);
        assert!(data_ticks.iter().all(|b| b.value.len() == 1));
    }

    #[test]
    fn csv_read_groups_same_timestamp_into_one_burst() {
        // read_test_multi.csv: timestamps 1001, 1002, 1003(×2), 1004, sentinel 9999
        // → 4 real ticks emitted (1003's two rows grouped), sentinel triggers the last real tick.
        let stream = csv_read(
            "src/adapters/csv/test_data/read_test_multi.csv",
            get_time,
            false,
        );
        let collected = stream.collect();
        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let real_ticks: Vec<_> = collected
            .peek_value()
            .into_iter()
            .filter(|b| b.value.iter().any(|r| r.1 != 0))
            .collect();
        // 4 distinct data timestamps
        assert_eq!(real_ticks.len(), 4);
        // The burst at timestamp 1003 should have 2 elements
        let burst_1003 = real_ticks.iter().find(|b| b.time == NanoTime::new(1003));
        assert!(burst_1003.is_some());
        assert_eq!(burst_1003.unwrap().value.len(), 2);
    }
}

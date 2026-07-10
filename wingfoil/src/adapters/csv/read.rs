use anyhow::Context;
use serde::de::DeserializeOwned;
use std::fs::File;
use std::rc::Rc;

use crate::nodes::TryIteratorStream;
use crate::queue::ValueAt;
use crate::types::*;

fn csv_iterator<T>(
    path: &str,
    get_time_func: impl Fn(&T) -> NanoTime + 'static,
    has_headers: bool,
) -> anyhow::Result<Box<dyn Iterator<Item = anyhow::Result<ValueAt<T>>>>>
where
    T: Element + DeserializeOwned + 'static,
{
    let file = File::open(path).with_context(|| format!("csv_read: failed to open {path}"))?;
    let path_owned = path.to_owned();
    let data = csv::ReaderBuilder::new()
        .has_headers(has_headers)
        .from_reader(file)
        .into_deserialize();
    Ok(Box::new(data.map(move |record: Result<T, csv::Error>| {
        let rec = record
            .with_context(|| format!("csv_read: failed to deserialize row from {path_owned}"))?;
        let time = get_time_func(&rec);
        Ok(ValueAt { value: rec, time })
    })))
}

/// Returns a stream that emits records from a CSV file as a [`Burst<T>`] per tick.
/// Multiple rows sharing the same timestamp are grouped into a single burst.
/// Use [`.collapse()`](crate::StreamOperators::collapse) when the source is
/// strictly ascending and you need a plain `T` per tick.
///
/// # Errors
///
/// Returns an error if the file cannot be opened. Rows that fail to
/// deserialize do not panic — the error is surfaced to graph execution when
/// the stream reaches that row.
pub fn csv_read<T>(
    path: &str,
    get_time_func: impl Fn(&T) -> NanoTime + 'static,
    has_headers: bool,
) -> anyhow::Result<Rc<dyn Stream<Burst<T>>>>
where
    T: Element + DeserializeOwned + 'static,
{
    Ok(TryIteratorStream::new(csv_iterator(path, get_time_func, has_headers)?).into_stream())
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
        // read_test.csv has 6 data rows; every row, including the last, must emit.
        let stream = csv_read("src/adapters/csv/test_data/read_test.csv", get_time, false).unwrap();
        let collected = stream.collect();
        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let all: Vec<u32> = collected
            .peek_value()
            .iter()
            .flat_map(|b| b.value.iter().map(|r| r.1))
            .collect();
        assert_eq!(all, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn csv_read_each_row_is_single_burst() {
        // read_test.csv: 6 data rows, each unique timestamp → 6 single-element bursts
        let stream = csv_read("src/adapters/csv/test_data/read_test.csv", get_time, false).unwrap();
        let collected = stream.collect();
        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let data_ticks = collected.peek_value();
        assert_eq!(data_ticks.len(), 6);
        assert!(data_ticks.iter().all(|b| b.value.len() == 1));
    }

    #[test]
    fn csv_read_missing_file_returns_error_with_context() {
        // A missing CSV file must surface a contextual error rather than panic.
        let result = csv_read::<Record>(
            "src/adapters/csv/test_data/does_not_exist.csv",
            get_time,
            false,
        );
        let err = result.expect_err("expected an error for a missing file");
        assert!(
            format!("{err:#}").contains("csv_read: failed to open"),
            "unexpected error message: {err:#}"
        );
    }

    #[test]
    fn csv_read_malformed_row_surfaces_error_not_panic() {
        // A row that cannot be deserialized into `Record` must fail the graph
        // run with a contextual error rather than aborting the process.
        let stream = csv_read::<Record>(
            "src/adapters/csv/test_data/read_test_malformed.csv",
            get_time,
            false,
        )
        .expect("file opens fine; the error is in a row");
        let result = stream
            .collect()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);
        let err = result.expect_err("expected a deserialize error");
        assert!(
            format!("{err:#}").contains("failed to deserialize row"),
            "unexpected error message: {err:#}"
        );
    }

    #[test]
    fn csv_read_groups_same_timestamp_into_one_burst() {
        // read_test_multi.csv: timestamps 1001, 1002, 1003(×2), 1004
        // → 4 ticks emitted (1003's two rows grouped into one burst).
        let stream = csv_read(
            "src/adapters/csv/test_data/read_test_multi.csv",
            get_time,
            false,
        )
        .unwrap();
        let collected = stream.collect();
        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        let real_ticks = collected.peek_value();
        // 4 distinct data timestamps
        assert_eq!(real_ticks.len(), 4);
        // The burst at timestamp 1003 should have 2 elements
        let burst_1003 = real_ticks.iter().find(|b| b.time == NanoTime::new(1003));
        assert!(burst_1003.is_some());
        assert_eq!(burst_1003.unwrap().value.len(), 2);
    }
}

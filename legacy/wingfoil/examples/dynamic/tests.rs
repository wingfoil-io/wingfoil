use super::*;

fn b(pairs: &[(&str, f64)]) -> BTreeMap<Instrument, Price> {
    pairs.iter().map(|&(k, v)| (k.into(), v)).collect()
}

/// Expected price-book state after each emission over 20 cycles.
///
/// Lifecycle (event_ticker, every period):
///   n=1  add inst1    n=2  add inst2    n=3  del inst1
///   n=4  add inst3    n=5  add inst4    n=6  del inst2
///   n=7  add inst5    n=8  add inst6    n=9  del inst3
///   n=10 add inst7    n=11 add inst8    n=12 del inst4
///   n=13 add inst9    n=14 add inst10   n=15 del inst5
///   n=16 add inst11   n=17 add inst12   n=18 del inst6
///   n=19 add inst13   n=20 add inst14
///
/// Price id = (n-1)/3 + (n-1)%3 + 1; rounded price = (id + n/100) * 100.
///
/// At n=3 only a deletion fires and the price is for inst3 (not yet live),
/// so the aggregator does not emit — 19 states total.
fn expected_book_states() -> Vec<BTreeMap<Instrument, Price>> {
    vec![
        b(&[("inst1", 101.0)]),                   // n=1
        b(&[("inst1", 101.0), ("inst2", 202.0)]), // n=2
        // n=3 skipped: del inst1, price for inst3 (not yet live)
        b(&[("inst2", 204.0)]),                   // n=4
        b(&[("inst2", 204.0), ("inst3", 305.0)]), // n=5
        b(&[("inst3", 305.0), ("inst4", 406.0)]), // n=6
        b(&[("inst3", 307.0), ("inst4", 406.0)]), // n=7
        b(&[("inst3", 307.0), ("inst4", 408.0)]), // n=8
        b(&[("inst4", 408.0), ("inst5", 509.0)]), // n=9
        b(&[("inst4", 410.0), ("inst5", 509.0)]), // n=10
        b(&[("inst4", 410.0), ("inst5", 511.0)]), // n=11
        b(&[("inst5", 511.0), ("inst6", 612.0)]), // n=12
        b(&[("inst5", 513.0), ("inst6", 612.0)]), // n=13
        b(&[("inst5", 513.0), ("inst6", 614.0)]), // n=14
        b(&[("inst6", 614.0), ("inst7", 715.0)]), // n=15
        b(&[("inst6", 616.0), ("inst7", 715.0)]), // n=16
        b(&[("inst6", 616.0), ("inst7", 717.0)]), // n=17
        b(&[("inst7", 717.0), ("inst8", 818.0)]), // n=18
        b(&[("inst7", 719.0), ("inst8", 818.0)]), // n=19
        b(&[("inst7", 719.0), ("inst8", 820.0)]), // n=20
    ]
}

/// Both tickers fire at t=0, period, 2·period, … so n=1..20 land at t=0..19s.
/// RunFor::Duration(period * 19) stops after the t=19s tick (n=20) but before
/// the t=20s tick (n=21) which would delete inst7.
#[test]
fn price_book_accumulation() {
    let period = std::time::Duration::from_secs(1);
    let (price_book, overflow_node) = build(period);
    let assertion = price_book.accumulate().finally(|states, _| {
        assert_eq!(states, expected_book_states());
        Ok(())
    });
    Graph::new(
        vec![assertion, overflow_node],
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Duration(period * 19),
    )
    .run()
    .unwrap();
}

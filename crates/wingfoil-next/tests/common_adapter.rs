//! Parity tests for the shared adapter helpers ported from legacy
//! `wingfoil::adapters::common` — the out-of-window row [`WindowFilter`] and its
//! [`TimeWindow`].
//!
//! The legacy module carries no `WindowFilter` unit tests of its own (its test
//! module only covers the `kdb`/`postgres`-gated `compute_time_slices`, which is
//! not part of this port — see the module docs), so these are new coverage that
//! pins the ported behaviour: half-open `[lo, hi)` containment, run-bound
//! clamping, and keep/drop accounting. `WindowFilter` is a pure helper (not a
//! graph op), so there are no tick times to assert.

use wingfoil_next::NanoTime;
use wingfoil_next::adapters::common::{TimeWindow, WindowFilter};

fn t(ns: u64) -> NanoTime {
    NanoTime::new(ns)
}

#[test]
fn time_window_contains_is_half_open() {
    let w = TimeWindow::clamp(t(10), t(20), t(0), t(100));
    assert!(!w.contains(t(9)), "below lo excluded");
    assert!(w.contains(t(10)), "lo included");
    assert!(w.contains(t(15)), "interior included");
    assert!(!w.contains(t(20)), "hi excluded (half-open)");
    assert!(!w.contains(t(21)), "above hi excluded");
}

#[test]
fn time_window_clamp_tightens_to_run_bounds() {
    // Candidate [0, 200) clamped to run bounds [50, 150).
    let w = TimeWindow::clamp(t(0), t(200), t(50), t(150));
    assert!(!w.contains(t(49)));
    assert!(w.contains(t(50)), "lo raised to start");
    assert!(w.contains(t(149)));
    assert!(!w.contains(t(150)), "hi lowered to end (exclusive)");
}

#[test]
fn window_filter_keeps_in_window_drops_out_of_window() {
    let window = TimeWindow::clamp(t(10), t(20), t(0), t(100));
    let mut filter = WindowFilter::new("test_adapter", window);

    // In-window rows kept; out-of-window rows dropped, in any order.
    assert!(!filter.keep(t(5)), "before window dropped");
    assert!(filter.keep(t(10)), "lo kept");
    assert!(filter.keep(t(19)), "interior kept");
    assert!(!filter.keep(t(20)), "hi dropped (half-open)");
    assert!(!filter.keep(t(25)), "after window dropped");

    // `finish` consumes and (here) would emit a warning for the 3 drops. No
    // return value to assert — exercising the path guards against a panic.
    filter.finish();
}

#[test]
fn window_filter_all_kept_no_warning_path() {
    let window = TimeWindow::clamp(t(0), t(100), t(0), t(100));
    let mut filter = WindowFilter::new("test_adapter", window);
    for ns in [0u64, 1, 50, 99] {
        assert!(filter.keep(t(ns)));
    }
    // dropped == 0, so `finish` takes the no-warning branch.
    filter.finish();
}

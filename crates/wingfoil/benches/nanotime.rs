//! Cost of reading the graph clock.
//!
//! Port of legacy `legacy/wingfoil/benches/nanotime.rs`, verbatim apart from the
//! import path. `NanoTime` is the *same type* on both trees — next re-exports
//! legacy's clock core rather than reimplementing it (see
//! `wingfoil::wingfoil`) — so this target exists for suite completeness
//! and to keep `cargo bench --bench nanotime` working after the cutover; the
//! two bars measure identical code and must not diverge.
//!
//! Run with: `cargo bench --manifest-path crates/wingfoil/Cargo.toml --bench nanotime`

use criterion::{Criterion, criterion_group, criterion_main};
use wingfoil::NanoTime;

fn bench(crit: &mut Criterion) {
    crit.bench_function("nanotime", |bencher| bencher.iter(NanoTime::now));
}

criterion_group!(benches, bench);
criterion_main!(benches);

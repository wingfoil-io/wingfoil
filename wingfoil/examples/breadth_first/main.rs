#![doc = include_str!("./README.md")]

//! ```python
//! import rx
//! import  time
//! from rx import operators as ops
//!
//! def test_perf(n):
//!     # need 2 ticks as combine_latest wont start ticking until
//!     # all inputs have ticked
//!     src = rx.from_iterable([1, 1]).pipe(ops.share())
//!     for i in range(n):
//!         src = rx.combine_latest(src, src) \
//!             .pipe(
//!                 ops.map(sum),
//!                 ops.share()
//!             )
//!     t0 = time.perf_counter()
//!     src.subscribe()
//!     t1 = time.perf_counter()
//!     return t1 - t0
//!
//! print("depth, seconds")
//! for i in range(16):
//!     print("%s, %.4f" % (i, test_perf(i)))
//! ```
//! This output can be plotted:
//! <div align="center">
//!   <img alt="diagram" src="../wingfoil/diagrams/rx_complexity.jpeg"/>
//! </div>
//! Of course ReactiveX developers can try and fight back against this behaviour.
//! In this trivial example, the rx zip operator could be used but this assumes
//! inputs are ticking in lock-step which wont generally be the case.   Throttling
//! and windowing operations could be used to attempt to mitigate this issue
//! but they do not lead to very satisfactory solutions.

use wingfoil::*;

fn main() {
    let mut source = constant(1_u128);
    for _ in 1..128 {
        source = add(&source, &source);
    }
    let cycles = source.count();
    cycles.run(RunMode::RealTime, RunFor::Forever).unwrap();
    println!("cycles {:?}", cycles.peek_value());
    println!("value {:?}", source.peek_value());
}

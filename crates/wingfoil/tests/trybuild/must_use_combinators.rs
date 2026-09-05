// `#[must_use]` on the transform/source combinators (#830) has to actually fire
// at the *call site*, and that is not a given: the attribute is inert when it
// sits on a trait-impl method (rustc resolves a method call to the trait's
// item), so an annotation that drifted onto the `__wf_fluent_*` expansions
// instead of the hand-written `StreamOps` / `SourceOps` declarations would
// silently stop warning. `deny` turns the lint into the compile error trybuild
// can pin.
//
// The other half of the pin is the sinks: `for_each` / `for_each_mut` / `print`
// / `logged` / `inspect` / `timed` / `finally` / `feedback` are called as bare
// statements all over this tree (`examples/adapters/fix/main.rs`), so they must
// **not** warn. They are exercised below under the same `deny`; if one of them
// ever grows the attribute, this file stops compiling and the expected stderr
// no longer matches.
#![deny(unused_must_use)]

use std::time::Duration;

use wingfoil::log::Level;
use wingfoil::prelude::*;

fn transforms(g: &GraphBuilder) {
    let count = g.ticker(Duration::from_nanos(10)).count();

    // Each of these wires a node that will cycle every tick for the whole run
    // and produce a value nobody reads.
    count.map(|i: &u64| i * 2);
    count.filter_value(|i: &u64| *i > 2);
    count.accumulate();
    count.with_time();
    count.fold(0u64, |acc: &mut u64, v: &u64| *acc += v);
    count.delay(Duration::from_nanos(10));
    count.audit(Duration::from_nanos(10));
    count.debounce(Duration::from_nanos(10));
    count.pairwise();
    count.try_map_filter(|i: &u64| Ok((i * 2, *i > 2)));
    count.filter_map(|i: &u64| (*i > 2).then(|| i * 2));
}

fn sources(g: &GraphBuilder) {
    // A source is the same bug with nothing upstream of it.
    g.ticker(Duration::from_nanos(10));
    g.constant(1u64);
    g.never();
    g.channel::<u64>();
}

// Sinks: no warning, so `deny(unused_must_use)` leaves them alone.
fn sinks(g: &GraphBuilder) {
    let count = g.ticker(Duration::from_nanos(10)).count();
    count.print();
    count.logged("count", Level::Info);
    count.inspect(|_: &u64| {});
    count.timed();
    count.for_each(|_: &u64| Ok(()));
    count.for_each_mut(Vec::<u64>::new(), |w: &mut Vec<u64>, v: &u64| {
        w.push(*v);
        Ok(())
    });
    count.finally(|_: &u64| Ok(()));

    let (loop_in, sink) = g.feedback::<u64>();
    let _ = loop_in;
    count.feedback(&sink);
}

fn start_with_is_must_use(g: &GraphBuilder) {
    g.constant(7u64).start_with(1);
}

fn main() {}

//! The **builder-less facade**: free source functions and a [`Signal<T>`] you
//! run directly, with no [`GraphBuilder`] or [`Runner`] in the caller's hands.
//!
//! This is the shape legacy wingfoil exposes (`nodes::ticker(period)`, then
//! `stream.run(..)` / `stream.peek_value()`), which is where the module came
//! from and why it was called `compat` until it was renamed. But the shape
//! stands on its own: for a short script or a doctest, wiring a graph, holding
//! a builder and threading a runner is ceremony, and this is the surface that
//! removes it. Legacy *compatibility* proper is carried by the `legacy/wingfoil`
//! crate over this engine, not here — see `docs/cutover-plan.md` item 1.4,
//! which is what rules on whether that facade survives the swap.
//!
//! ```
//! use std::time::Duration;
//! use wingfoil::{NanoTime, RunFor, RunMode};
//! use wingfoil::signal::ticker;
//!
//! let counted = ticker(Duration::from_nanos(100)).count();
//! counted.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))?;
//! assert_eq!(5, counted.peek_value());
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! A [`Signal<T>`] wraps the fluent [`Stream`] plus the shared graph and a slot
//! for the [`Runner`] produced by `run`, so every combinator is the same
//! forward: unwrap, call the fluent method, re-wrap.
//!
//! **That forwarding is generated.** It was hand-written while the module was a
//! proof of concept, and it drifted — 15 of [`StreamOps`]' 41 methods were
//! simply missing, one per op nobody remembered to come back for. Each
//! `#[op(build = x, fluent)]` now emits `__wf_signal_x!` alongside its fluent
//! twin, and the `impl` blocks below invoke it, so an op cannot land in the
//! catalog and skip this facade. What stays hand-written is what is not a plain
//! forward: the source free functions (they *make* the graph), `run` /
//! `peek_value` (the facade's whole point), and the few combinators whose
//! `Signal` signature genuinely differs from the fluent one.

use std::cell::RefCell;
use std::fmt::Debug;
use std::ops::{Not, Sub};
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use wingfoil::{NanoTime, RunFor, RunMode};

use crate::fluent::{GraphBuilder, SourceOps, Stream, StreamOps};
use crate::interp::Runner;

/// A stream in an implicit graph, with the legacy `run` / `peek_value`
/// ergonomics. Combinators mirror the legacy `StreamOperators`.
pub struct Signal<T> {
    stream: Stream<T>,
    graph: GraphBuilder,
    /// The runner produced by [`Signal::run`], shared by every signal in the
    /// graph so `peek_value` works whichever one you call it on.
    runner: Rc<RefCell<Option<Runner>>>,
}

impl<T> Clone for Signal<T> {
    fn clone(&self) -> Self {
        Self {
            stream: self.stream.clone(),
            graph: self.graph.clone(),
            runner: self.runner.clone(),
        }
    }
}

/// A source that ticks at a fixed interval — the legacy free function.
pub fn ticker(period: Duration) -> Signal<()> {
    let graph = GraphBuilder::new();
    let stream = graph.ticker(period);
    Signal {
        stream,
        graph,
        runner: Rc::new(RefCell::new(None)),
    }
}

/// A source that ticks once with `value` — the legacy free function.
pub fn constant<T: Clone + Default + 'static>(value: T) -> Signal<T> {
    let graph = GraphBuilder::new();
    let stream = graph.constant(value);
    Signal {
        stream,
        graph,
        runner: Rc::new(RefCell::new(None)),
    }
}

impl<T> Signal<T> {
    /// The [`Stream`] this signal wraps, for wiring the next node onto.
    ///
    /// This — with [`wrap`](Self::wrap) — is the **`#[op(fluent)]` facade
    /// seam**: every combinator the macro generates is `as_stream` on the
    /// receiver and each edge, wire the op, `wrap` the result back into this
    /// graph. Naming the pair is what keeps the generated code off the private
    /// fields, so the facade's representation stays free to change as long as
    /// these two hold.
    ///
    /// `pub(crate)` is the widest this needs to be, and it is not a
    /// restriction: `Signal`'s combinators are *inherent* methods, so only
    /// this crate can add one. Callers outside it hold the graph explicitly
    /// and use [`Stream`] directly.
    pub(crate) fn as_stream(&self) -> &Stream<T> {
        &self.stream
    }

    /// Put `stream` into this signal's graph — the other half of the seam
    /// described on [`as_stream`](Self::as_stream). The new signal shares the
    /// graph and the runner slot, so [`run`](Signal::run) on any signal in the
    /// graph makes [`peek_value`](Signal::peek_value) work on all of them.
    pub(crate) fn wrap<B>(&self, stream: Stream<B>) -> Signal<B> {
        Signal {
            stream,
            graph: self.graph.clone(),
            runner: self.runner.clone(),
        }
    }
}

/// Every combinator that is a **plain forward** to the fluent method of the
/// same name, generated from the op catalog by `#[op(build = x, fluent)]`.
///
/// They share one `impl` block because each generated method carries its own
/// `where` clause, taken from the op — `difference` asks for `T: Sub`, `print`
/// for `T: Debug` — so the block itself needs only `T: 'static`, exactly as
/// `impl<T: 'static> StreamOps<T> for Stream<T>` does.
///
/// **Adding an op to the catalog adds it here.** Nothing in this file needs
/// editing: `#[op(fluent)]` emits `__wf_signal_<name>!` and the only cost is
/// the one line below. The methods the facade keeps by hand are the ones that
/// are *not* plain forwards — see the blocks that follow.
impl<T: 'static> Signal<T> {
    __wf_signal_map!(T);
    __wf_signal_try_map!(T);
    __wf_signal_map_filter!(T);
    __wf_signal_fold!(T);
    __wf_signal_for_each!(T);
    __wf_signal_ticked_at!(T);
    __wf_signal_ticked_at_elapsed!(T);
    __wf_signal_filter!(T);
    __wf_signal_accumulate!(T);
    __wf_signal_sample!(T);
    __wf_signal_merge!(T);
    __wf_signal_limit!(T);
    __wf_signal_throttle!(T);
    __wf_signal_window!(T);
    __wf_signal_buffer!(T);
    __wf_signal_inspect!(T);
    __wf_signal_timed!(T);
    __wf_signal_finally!(T);
    __wf_signal_delay!(T);
    __wf_signal_distinct!(T);
    __wf_signal_drop_small_change!(T);
    __wf_signal_print!(T);
    __wf_signal_difference!(T);
    __wf_signal_join!(T);
    __wf_signal_join3!(T);
    __wf_signal_join_passive!(T);
    __wf_signal_try_join!(T);
    __wf_signal_try_join3!(T);
    __wf_signal_try_join_passive!(T);
}

impl<T: 'static> Signal<T> {
    /// Map and filter with an `Option` (the legacy `filter_map`): tick the
    /// returned `Some`, drop `None`. Delegates to the fluent
    /// [`map_filter`](StreamOps::map_filter).
    pub fn filter_map<B, F>(&self, f: F) -> Signal<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> Option<B> + 'static,
    {
        self.wrap(self.stream.map_filter(move |v| match f(v) {
            Some(out) => (out, true),
            None => (B::default(), false),
        }))
    }

    /// Emit the result of `f()` on each tick, ignoring the value (the legacy
    /// `produce`). Sugar over [`map`](StreamOps::map).
    pub fn produce<B, F>(&self, f: F) -> Signal<B>
    where
        B: Clone + Default + 'static,
        F: Fn() -> B + 'static,
    {
        self.wrap(self.stream.map(move |_| f()))
    }

    /// Collapse a burst/iterator value into a single tick of its **last** item
    /// (the legacy `collapse`); stays quiet when the iterator is empty.
    pub fn collapse<OUT>(&self) -> Signal<OUT>
    where
        T: Clone + IntoIterator<Item = OUT>,
        OUT: Clone + Default + 'static,
    {
        self.wrap(self.stream.collapse())
    }

    /// Pair each value with the current engine time: `(time, value)`.
    pub fn with_time(&self) -> Signal<(NanoTime, T)>
    where
        T: Clone,
    {
        self.wrap(self.stream.with_time())
    }

    /// Run the graph to its bound, storing the runner for `peek_value`.
    ///
    /// **Re-runnable**: the graph is built once (on the first call) and the
    /// [`Runner`] is retained, so a second `run` reuses it —
    /// [`Runner::run`](crate::interp::Runner::run) restores every node's state
    /// and value slot to its wiring-time initial value first, giving each run
    /// independent, reproducible results (legacy's setup-per-run semantics).
    /// This is what the wingfoil-python pytest suite — and any legacy-idiom
    /// code that runs a stream more than once — depends on.
    pub fn run(&self, run_mode: RunMode, run_for: RunFor) -> Result<()> {
        let mut slot = self.runner.borrow_mut();
        if slot.is_none() {
            *slot = Some(self.graph.build());
        }
        slot.as_mut()
            .expect("runner just built")
            .run(run_mode, run_for)
    }

    /// The stream's current value after a [`run`](Signal::run).
    ///
    /// # Panics
    ///
    /// Panics if called before [`run`](Signal::run): there is no value to read
    /// until the graph has run. This mirrors the legacy `Stream::peek_value`,
    /// which is infallible (returns `T`, not `Result<T>`) so the facade stays
    /// drop-in compatible; the precondition is documented and enforced with an
    /// explanatory panic rather than a bare out-of-bounds one.
    pub fn peek_value(&self) -> T
    where
        T: Clone + Default,
    {
        self.runner
            .borrow()
            .as_ref()
            .expect("Signal::run must be called before Signal::peek_value")
            .value(&self.stream)
    }
}

impl<T: Clone + Default + 'static> Signal<T> {
    /// Drop values contingent on a predicate (the legacy `filter_value`):
    /// keep a value only when `predicate` returns true. Delegates to the
    /// fluent [`map_filter`](StreamOps::map_filter).
    pub fn filter_value<F>(&self, predicate: F) -> Signal<T>
    where
        F: Fn(&T) -> bool + 'static,
    {
        self.wrap(self.stream.map_filter(move |v| (v.clone(), predicate(v))))
    }

    /// Fold values into an accumulator seeded from `T::default()`, applying
    /// `f(acc, value)` (the legacy `reduce`). Delegates to the fluent
    /// [`fold`](StreamOps::fold).
    pub fn reduce<F>(&self, f: F) -> Signal<T>
    where
        F: Fn(&T, &T) -> T + 'static,
    {
        self.wrap(self.stream.fold(T::default(), move |acc, val| {
            *acc = f(acc, val);
        }))
    }
}

impl<T: Clone + Default + Debug + 'static> Signal<T> {
    /// Pass each value through unchanged, logging it at `level` under `label`
    /// (the legacy `logged`).
    ///
    /// Hand-written rather than generated for the reason recorded in
    /// `tests/op_completeness.rs`: `Logged`'s `Cfg` is `(String, log::Level)`
    /// but the method takes a `&str` so a `format!`-built label works, and the
    /// generator forwards the config type verbatim.
    pub fn logged(&self, label: &str, level: log::Level) -> Signal<T> {
        self.wrap(self.stream.logged(label, level))
    }
}

impl<T: Clone + Default + PartialEq + 'static> Signal<T> {
    /// [`delay`](Signal::delay) with a reset trigger (the legacy
    /// `delay_with_reset`): when `trigger` ticks, the output snaps to the
    /// current value and any pending (delayed) values are dropped. `trigger`
    /// is read for its tick only, so its value type is irrelevant.
    pub fn delay_with_reset<U: 'static>(&self, delay: Duration, trigger: &Signal<U>) -> Signal<T> {
        self.wrap(self.stream.delay_with_reset(delay, &trigger.stream))
    }
}

impl<A, B> Signal<(A, B)>
where
    A: Clone + Default + 'static,
    B: Clone + Default + 'static,
{
    /// Decompose a signal of pairs into its two component signals (the legacy
    /// `split`). Both branches tick whenever the source does.
    pub fn split(&self) -> (Signal<A>, Signal<B>) {
        let (a, b) = self.stream.split();
        (self.wrap(a), self.wrap(b))
    }
}

impl<T: Clone + Default + 'static> Signal<Option<T>> {
    /// Drop `None` values, yielding a `Signal<T>` of just the `Some` payloads
    /// (the legacy `filter_none`). Delegates to the fluent
    /// [`map_filter`](StreamOps::map_filter).
    pub fn filter_none(&self) -> Signal<T> {
        self.wrap(self.stream.map_filter(|opt: &Option<T>| match opt.clone() {
            Some(v) => (v, true),
            None => (T::default(), false),
        }))
    }
}

impl<T: Clone + Default + Not<Output = T> + 'static> Signal<T> {
    /// Negate each value (`!value`) — sugar over `map`.
    pub fn not(&self) -> Signal<T> {
        self.wrap(self.stream.not())
    }
}

impl Signal<()> {
    /// Running count of ticks: 1, 2, 3, ...
    pub fn count(&self) -> Signal<u64> {
        self.wrap(self.stream.count())
    }
}

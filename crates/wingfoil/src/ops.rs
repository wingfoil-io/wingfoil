//! The core op vocabulary. Each op is a zero-sized *witness type* carrying
//! semantics in associated functions — never instantiated. Compare each
//! `cycle` body with the corresponding `MutableNode` impl in the main crate:
//! the logic is identical, but here it is written once and executed by every
//! engine, interpreted or compiled.
//!
//! Every op carries `#[op(build = name)]`, which generates *both* engines'
//! wiring from the op's declared shape: the `nitro!` forwarder functions
//! (`__wf_op_<name>_*`) that all compiled/nested emission dispatches through,
//! and the interpreted [`Builder`](crate::interp::Builder) method, with the
//! node label derived from `type_name`. That covers every shape in the
//! catalog — sources (`In = ()`), single- and multi-input ops, edges read with
//! their tick flag (delay, merge), lifecycle hooks (ticker, window, timed,
//! finally), `passive = [..]` non-activating edges (sample), and `init_arg`
//! seeded accumulators (fold). `no_builder` is left for the one op whose
//! interpreted signature deliberately differs from its shape — `with_time`,
//! which seeds its value slot from the input so it never needs
//! `Out: Default`. (`bimap`/`trimap` are *extra* hand-written `Builder`
//! methods over [`Join`]/[`Join3`], taking runtime active/passive flags rather
//! than a compile-time mask; they sit alongside the generated `join` /
//! `join_passive` / `join3`, they are not opt-outs.)
//! See `docs/adding-an-op.md` for the full recipe.

use std::collections::VecDeque;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::ops::Sub;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::op::{Activation, Ctx, Op, Tick};
use crate::runtime::burst::Burst;
use wingfoil::{NanoTime, TimeQueue};
use wingfoil_derive::op;

/// Ticks at a fixed interval, anchored to its first activation to avoid
/// drift. `Cfg` = interval, `State` = the converted period plus the last
/// scheduled time.
pub struct Ticker;

/// [`Ticker`] state: the period converted to engine time **once** in `start`
/// (converting per cycle measurably slows dense graphs), plus the last
/// scheduled time (drift anchoring).
#[derive(Default)]
pub struct TickerState {
    period: NanoTime,
    next: Option<NanoTime>,
}

#[op(build = ticker, fluent)]
impl Op for Ticker {
    /// The interval as passed at the call site (`Duration`, per the uniform
    /// arg-is-the-config convention); converted to engine time in `start`.
    type Cfg = Duration;
    type State = TickerState;
    type In<'a> = ();
    type Out = ();
    const ACTIVATION: Activation = Activation::SCHEDULES;

    fn cycle(
        _cfg: &mut Duration,
        state: &mut TickerState,
        _input: (),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<()>> {
        let next = match state.next {
            Some(t) => t + state.period,
            None => ctx.time() + state.period,
        };
        state.next = Some(next);
        ctx.schedule(next);
        Ok(Tick::Value(()))
    }

    fn start(cfg: &mut Duration, state: &mut TickerState, ctx: &mut Ctx<'_>) -> Result<()> {
        state.period = NanoTime::from(*cfg);
        ctx.schedule(ctx.start_time());
        Ok(())
    }
}

/// Ticks once with a fixed value, on the first cycle.
pub struct Const<T>(PhantomData<T>);

#[op(build = constant, fluent)]
impl<T: Clone + 'static> Op for Const<T> {
    type Cfg = T;
    type State = ();
    type In<'a> = ();
    type Out = T;
    const ACTIVATION: Activation = Activation::SCHEDULES;

    fn cycle(cfg: &mut T, _state: &mut (), _input: (), _ctx: &mut Ctx<'_>) -> Result<Tick<T>> {
        Ok(Tick::Value(cfg.clone()))
    }

    fn start(_cfg: &mut T, _state: &mut (), ctx: &mut Ctx<'_>) -> Result<()> {
        ctx.schedule(ctx.start_time());
        Ok(())
    }
}

/// Applies a closure to its input. The closure *is* the config — a type
/// parameter, never boxed, so compiled engines monomorphize straight through
/// it.
///
/// The bound is `Fn`, not `FnMut`, and that is a correctness contract, not a
/// convenience: compiled expansions re-create closure configs per cycle, so
/// a closure mutating its captures would silently reset there while
/// persisting interpreted — the engines would drift. `Fn` makes that a
/// compile error in both. Per-node state belongs in [`Fold`]'s accumulator.
pub struct Map<A, B, F>(PhantomData<(A, B, F)>);

#[op(build = map, fluent)]
impl<A, B, F> Op for Map<A, B, F>
where
    A: 'static,
    B: Clone + 'static,
    F: Fn(&A) -> B + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a A,);
    type Out = B;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut F, _state: &mut (), input: (&A,), _ctx: &mut Ctx<'_>) -> Result<Tick<B>> {
        Ok(Tick::Value(cfg(input.0)))
    }
}

/// Applies a *fallible* closure to its input, propagating any error to abort
/// the run with context. The `try_` counterpart to [`Map`]; the closure is
/// `Fn` for the same drift-safety reason (see [`Map`]).
pub struct TryMap<A, B, F>(PhantomData<(A, B, F)>);

#[op(build = try_map, fluent)]
impl<A, B, F> Op for TryMap<A, B, F>
where
    A: 'static,
    B: Clone + 'static,
    F: Fn(&A) -> Result<B> + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a A,);
    type Out = B;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut F, _state: &mut (), input: (&A,), _ctx: &mut Ctx<'_>) -> Result<Tick<B>> {
        Ok(Tick::Value(cfg(input.0)?))
    }
}

/// Emits a value with a per-value tick decision: the closure returns
/// `(value, emit?)`, so it maps and filters in one pass. The `filter_map` of
/// the catalog. `Fn`, like [`Map`].
pub struct MapFilter<A, B, F>(PhantomData<(A, B, F)>);

#[op(build = map_filter, fluent)]
impl<A, B, F> Op for MapFilter<A, B, F>
where
    A: 'static,
    B: Clone + 'static,
    F: Fn(&A) -> (B, bool) + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a A,);
    type Out = B;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut F, _state: &mut (), input: (&A,), _ctx: &mut Ctx<'_>) -> Result<Tick<B>> {
        let (value, emit) = cfg(input.0);
        Ok(if emit {
            Tick::Value(value)
        } else {
            Tick::Quiet
        })
    }
}

/// Suppresses consecutive duplicate values: emits the first value, then only
/// when it changes. State is `Option<T>` (not the default-initialised output)
/// so a genuine first value equal to `T::default()` still ticks.
pub struct Distinct<T>(PhantomData<T>);

#[op(build = distinct, fluent)]
impl<T: Clone + PartialEq + 'static> Op for Distinct<T> {
    type Cfg = ();
    type State = Option<T>;
    type In<'a> = (&'a T,);
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut Option<T>,
        input: (&T,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        let curr = input.0;
        if state.as_ref() == Some(curr) {
            Ok(Tick::Quiet)
        } else {
            *state = Some(curr.clone());
            Ok(Tick::Value(curr.clone()))
        }
    }
}

/// Suppresses ticks while the change from the **last emitted** value is
/// judged small: `is_small(current, last_emitted)` returning `true` drops the
/// tick. The first value always ticks (there is nothing to compare against),
/// and the reference stays pinned to the last value actually emitted, so a
/// slow drift of individually-small steps still eventually ticks.
///
/// The generalisation of [`Distinct`], which is this op with an equality
/// predicate. State is `Option<T>` for the same reason: a genuine first value
/// equal to `T::default()` must still tick.
///
/// The predicate *is* the config — a type parameter, `Fn` not `FnMut`, for
/// the drift-safety reason spelled out on [`Map`].
///
/// Ports legacy `wingfoil::nodes::DropSmallChangeStream`.
pub struct DropSmallChange<T, F>(PhantomData<(T, F)>);

#[op(build = drop_small_change, fluent)]
impl<T, F> Op for DropSmallChange<T, F>
where
    T: Clone + 'static,
    F: Fn(&T, &T) -> bool + 'static,
{
    type Cfg = F;
    type State = Option<T>;
    type In<'a> = (&'a T,);
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut F,
        state: &mut Option<T>,
        input: (&T,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        let current = input.0;
        let should_emit = match state.as_ref() {
            None => true,
            Some(previous) => !cfg(current, previous),
        };
        if should_emit {
            *state = Some(current.clone());
            Ok(Tick::Value(current.clone()))
        } else {
            Ok(Tick::Quiet)
        }
    }
}

/// Emits the successive difference `value - previous`. Quiet on the first
/// value (no previous to subtract).
pub struct Difference<T>(PhantomData<T>);

#[op(build = difference, fluent)]
impl<T> Op for Difference<T>
where
    T: Clone + Sub<Output = T> + 'static,
{
    type Cfg = ();
    type State = Option<T>;
    type In<'a> = (&'a T,);
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut Option<T>,
        input: (&T,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        let value = input.0.clone();
        let out = match state.take() {
            Some(prev) => Tick::Value(value.clone() - prev),
            None => Tick::Quiet,
        };
        *state = Some(value);
        Ok(out)
    }
}

/// Negates each value (`!value`).
///
/// Was fluent-only sugar over [`Map`] (`map(|v| !v.clone())`) until it became
/// an op. The reason is the one [`Count`] and [`Accumulate`] record, and
/// `merge_all` before them: sugar that lives only at the fluent layer is
/// invisible to `nitro!`, so it cannot appear in a compiled graph — and if the
/// macro were taught to desugar it instead, the expansion would exist in two
/// places that can drift. One op, both layers.
///
/// The trait is spelled `std::ops::Not` in full so the witness type can keep
/// the op's name without importing a colliding trait — [`Difference`] is the
/// same shape over `Sub`.
pub struct Not<T>(PhantomData<T>);

#[op(build = not, fluent)]
impl<T> Op for Not<T>
where
    T: Clone + std::ops::Not<Output = T> + 'static,
{
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a T,);
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(_cfg: &mut (), _state: &mut (), input: (&T,), _ctx: &mut Ctx<'_>) -> Result<Tick<T>> {
        Ok(Tick::Value(!input.0.clone()))
    }
}

/// Collapses an iterable value into a single tick of its **last** item, staying
/// [`Quiet`](Tick::Quiet) when the iterable is empty (the legacy `collapse`).
///
/// # This discards data — do not put it on an ingest path
///
/// "Last item" means **every other item is dropped**, silently. That is fine
/// for a latest-value-wins signal (a price, a gauge reading) and wrong for
/// anything where each value is an event in its own right: orders, fills,
/// execution reports, control messages, log lines.
///
/// The trap is that it looks safe in testing. A source that produces one value
/// per graph cycle yields single-item bursts, and `collapse` is then lossless.
/// Bursts only grow when a producer outruns the cycle — so the loss appears
/// exactly under load, and never before. `wingfoil`'s own `trading_e2e`
/// showcase shipped this bug on all six of its ingest paths, and legacy still
/// does.
///
/// The reason it is reached for so readily is that adapters emit
/// `Stream<Burst<T>>` while most combinators want `Stream<T>`, and this is the
/// one-step bridge. Prefer staying burst-shaped instead:
///
/// | Instead of | Use |
/// |---|---|
/// | `.collapse()` then `map`/`fold`/`for_each` | the same op, iterating the burst inside the closure |
/// | `.collapse()` then `.stamp::<S>()` | [`stamp_each`](crate::latency::LatencyBurstStreamOps::stamp_each) |
/// | `.collapse()` then `.latency_report(..)` | `.latency_report(..)` — it has a `Stream<Burst<P>>` impl |
/// | `.collapse()` then `.otlp_spans(..)` | `.otlp_spans(..)` — likewise |
/// | `.collapse()` then `.web_pub(..)` | `web_pub_each` — same wire format, one frame per value |
/// | `.collapse()` then `.accumulate()` | [`collapse_accumulate`](crate::fluent::Stream::collapse_accumulate) |
///
/// Reach for `collapse` when you *mean* "the newest value at this instant",
/// and say so at the call site.
///
/// Promoted out of fluent-only sugar over [`MapFilter`] for the same reason as
/// [`Not`] — the quiet-on-empty rule is a real tick-suppression contract, and
/// it now lives in one place that every engine executes.
pub struct Collapse<T, OUT>(PhantomData<(T, OUT)>);

#[op(build = collapse, fluent)]
impl<T, OUT> Op for Collapse<T, OUT>
where
    T: Clone + IntoIterator<Item = OUT> + 'static,
    OUT: Clone + 'static,
{
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a T,);
    type Out = OUT;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&T,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<OUT>> {
        Ok(match input.0.clone().into_iter().last() {
            Some(last) => Tick::Value(last),
            None => Tick::Quiet,
        })
    }
}

/// Passes through the first `limit` values, then stays quiet. `Cfg` is the
/// limit; `State` the count emitted so far.
pub struct Limit<T>(PhantomData<T>);

#[op(build = limit, fluent)]
impl<T: Clone + 'static> Op for Limit<T> {
    type Cfg = u32;
    type State = u32;
    type In<'a> = (&'a T,);
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut u32, state: &mut u32, input: (&T,), _ctx: &mut Ctx<'_>) -> Result<Tick<T>> {
        if *state >= *cfg {
            Ok(Tick::Quiet)
        } else {
            *state += 1;
            Ok(Tick::Value(input.0.clone()))
        }
    }
}

/// Rate-limits: emits the first value, then suppresses until at least
/// `interval` has passed since the last emit. `Cfg` = the interval as passed
/// at the call site (`Duration`, per the uniform arg-is-the-config
/// convention; converted to engine time in `cycle`), `State` = last emit time.
pub struct Throttle<T>(PhantomData<T>);

#[op(build = throttle, fluent)]
impl<T: Clone + 'static> Op for Throttle<T> {
    type Cfg = Duration;
    type State = Option<NanoTime>;
    type In<'a> = (&'a T,);
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut Duration,
        state: &mut Option<NanoTime>,
        input: (&T,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        let interval = NanoTime::from(*cfg);
        let now = ctx.time();
        let emit = match *state {
            None => true,
            Some(last) => now - last >= interval,
        };
        Ok(if emit {
            *state = Some(now);
            Tick::Value(input.0.clone())
        } else {
            Tick::Quiet
        })
    }
}

/// Observes each value with a side-effecting closure and passes it through
/// unchanged (always ticks). The debug-tap of the catalog; the observer is
/// infallible `Fn` (contrast [`Sink`], the fallible outbound edge).
pub struct Inspect<A, F>(PhantomData<(A, F)>);

#[op(build = inspect, fluent)]
impl<A, F> Op for Inspect<A, F>
where
    A: Clone + 'static,
    F: Fn(&A) + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a A,);
    type Out = A;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut F, _state: &mut (), input: (&A,), _ctx: &mut Ctx<'_>) -> Result<Tick<A>> {
        cfg(input.0);
        Ok(Tick::Value(input.0.clone()))
    }
}

/// Passes each value through unchanged, printing it (`{value:?}` on its own
/// line) to stdout as it ticks. The legacy `print` node. Stateless.
///
/// **Deviation from legacy (justified — tracked as D8 in
/// `docs/planning/deviation-register.md`).** Legacy `print` *buffers* every value
/// and prints the whole buffer at `Drop` (teardown); this twin prints each
/// value immediately in `cycle`. The observable value stream is identical —
/// `print` is a pass-through — only the diagnostic emission differs. Per-tick
/// printing (a) drops the unbounded `Vec` buffer legacy grows one entry per
/// tick for the whole run, (b) streams output as the run progresses rather
/// than in one dump at the end, and (c) still prints what was seen if a later
/// cycle aborts the run. Shedding the teardown hook also leaves `print` the
/// same plain single-input shape as `map` / `limit`.
pub struct Print<T>(PhantomData<T>);

#[op(build = print, fluent)]
impl<T: Clone + Debug + 'static> Op for Print<T> {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a T,);
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(_cfg: &mut (), _state: &mut (), input: (&T,), _ctx: &mut Ctx<'_>) -> Result<Tick<T>> {
        println!("{:?}", input.0);
        Ok(Tick::Value(input.0.clone()))
    }
}

/// Logs each value as it ticks — `"{time} {label} {value:?}"` at `level`, via
/// the `log` crate (target `"wingfoil"`) — and passes it through unchanged.
/// The legacy `logged` debug tap. `Cfg` = `(label, level)`; stateless.
///
/// Deviations from legacy (both benign; the value stream is a pass-through,
/// only the diagnostic differs). Legacy `logged` (a) skips wiring the node
/// entirely when `level` is disabled — a wiring-time `log_enabled!` short
/// circuit that returns the source unchanged — and (b) reads the tick time via
/// a `bimap` with `ticked_at_elapsed`. The wingfoil twin (a) always wires the
/// node, leaning on `log!`'s own internal enabled-check so a disabled level
/// still costs only that check, and (b) reads the time straight off
/// [`Ctx::time`]. Always wiring keeps the interpreted and compiled engines
/// identical — a wiring-time skip cannot be expressed in `compiled()`.
pub struct Logged<T>(PhantomData<T>);

#[op(build = logged)]
impl<T: Clone + Debug + 'static> Op for Logged<T> {
    type Cfg = (String, log::Level);
    type State = ();
    type In<'a> = (&'a T,);
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut (String, log::Level),
        _state: &mut (),
        input: (&T,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        let (label, level) = &*cfg;
        log::log!(target: "wingfoil", *level, "{} {label} {:?}", ctx.time().pretty(), input.0);
        Ok(Tick::Value(input.0.clone()))
    }
}

/// Pending state for a [`Timed`] op: the tick count and the wall-clock start
/// instant (set at [`start`](Op::start)).
///
/// Carries the source value type `T` as a `PhantomData` marker purely so the
/// `#[op]`-generated `start`/`stop` forwarders can anchor `T` through the
/// `&mut State` argument (`Timed`'s `Cfg`/`Out` do not otherwise pin it in a
/// hook that ignores the value) — the lifecycle behaviour is `T`-independent.
pub struct TimedState<T> {
    cycles: u64,
    wall_start: Option<Instant>,
    _marker: PhantomData<T>,
}

impl<T> Default for TimedState<T> {
    fn default() -> Self {
        Self {
            cycles: 0,
            wall_start: None,
            _marker: PhantomData,
        }
    }
}

/// Passes its source value through unchanged, recording the wall-clock start
/// at [`start`](Op::start) and printing a performance summary at
/// [`stop`](Op::stop) (tick count, wall-clock duration, per-tick average, and
/// the engine-time span covered). The legacy `timed` node.
///
/// Deviations from legacy (observable value stream is identical — a
/// pass-through — only the diagnostic differs): the summary goes to `stderr`
/// via `eprintln!` rather than `log::info!` (wingfoil has no `log`
/// dependency), and — since [`Ctx`] exposes no run mode — it always reports
/// the wall-vs-engine speedup rather than branching on historical/realtime.
pub struct Timed<T>(PhantomData<T>);

#[op(build = timed, fluent)]
impl<T: Clone + 'static> Op for Timed<T> {
    type Cfg = ();
    type State = TimedState<T>;
    type In<'a> = (&'a T,);
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn start(_cfg: &mut (), state: &mut TimedState<T>, _ctx: &mut Ctx<'_>) -> Result<()> {
        state.wall_start = Some(Instant::now());
        Ok(())
    }

    fn cycle(
        _cfg: &mut (),
        state: &mut TimedState<T>,
        input: (&T,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        state.cycles += 1;
        Ok(Tick::Value(input.0.clone()))
    }

    fn stop(_cfg: &mut (), state: &mut TimedState<T>, ctx: &mut Ctx<'_>) -> Result<()> {
        let engine_elapsed = Duration::from(ctx.time() - ctx.start_time());
        let wall = state
            .wall_start
            .map(|s| s.elapsed())
            .unwrap_or(engine_elapsed);
        let cycles = state.cycles;
        let avg = Duration::from_nanos((wall.as_nanos() / u128::from(cycles.max(1))) as u64);
        let speedup = if wall.as_secs_f64() > 0.0 {
            engine_elapsed.as_secs_f64() / wall.as_secs_f64()
        } else {
            f64::INFINITY
        };
        eprintln!(
            "{cycles} ticks processed in {wall:?}, {avg:?} average.   \
             Covered {engine_elapsed:?} of engine time (x{speedup:.1})."
        );
        Ok(())
    }
}

/// Buffers values and flushes them as a `Vec` on each fixed time boundary
/// (`interval`), plus a final flush on the last cycle. `Cfg` = the interval as
/// passed at the call site (`Duration`, per the uniform arg-is-the-config
/// convention; converted to engine time in `start`/`cycle`); `State` holds the
/// next boundary and the pending buffer.
pub struct Window<T>(PhantomData<T>);

/// Pending state for a [`Window`] op.
pub struct WindowState<T> {
    next_window: NanoTime,
    buffer: Vec<T>,
}

impl<T> Default for WindowState<T> {
    fn default() -> Self {
        Self {
            next_window: NanoTime::ZERO,
            buffer: Vec::new(),
        }
    }
}

/// A [`Window`]'s interval in engine time, floored at one nanosecond.
///
/// The floor is not cosmetic: `cycle` advances the boundary with
/// `while next_window <= now { next_window += interval }`, which **never
/// terminates** for a zero interval — `window(Duration::ZERO)` hung the run
/// outright. One nanosecond is the engine clock's own resolution and the limit
/// the behaviour tends to as the window shrinks (every cycle is its own
/// boundary, so each value flushes alone), and it matches how the rest of the
/// catalog handles a degenerate size — the rolling ops all clamp with
/// `(*cfg).max(1)`.
fn window_interval(cfg: &Duration) -> NanoTime {
    NanoTime::from(*cfg).max(NanoTime::new(1))
}

#[op(build = window, fluent)]
impl<T: Clone + 'static> Op for Window<T> {
    type Cfg = Duration;
    type State = WindowState<T>;
    type In<'a> = (&'a T,);
    type Out = Vec<T>;
    const ACTIVATION: Activation = Activation::NONE;

    fn start(cfg: &mut Duration, state: &mut WindowState<T>, ctx: &mut Ctx<'_>) -> Result<()> {
        state.next_window = ctx.time() + window_interval(cfg);
        Ok(())
    }

    fn cycle(
        cfg: &mut Duration,
        state: &mut WindowState<T>,
        input: (&T,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Vec<T>>> {
        let interval = window_interval(cfg);
        let now = ctx.time();
        let mut out = None;
        if now >= state.next_window {
            if !state.buffer.is_empty() {
                out = Some(std::mem::take(&mut state.buffer));
            }
            // Advance the boundary past `now`, regardless of data.
            while state.next_window <= now {
                state.next_window = state.next_window + interval;
            }
        }
        state.buffer.push(input.0.clone());
        if out.is_none() && ctx.is_last_cycle() && !state.buffer.is_empty() {
            out = Some(std::mem::take(&mut state.buffer));
        }
        Ok(match out {
            Some(v) => Tick::Value(v),
            None => Tick::Quiet,
        })
    }
}

/// Buffers values and flushes them as a `Vec` once `capacity` accumulate,
/// plus a final flush on the last cycle. The count-based counterpart to
/// [`Window`]. `Cfg` = capacity, `State` = pending buffer.
pub struct Buffer<T>(PhantomData<T>);

#[op(build = buffer, fluent)]
impl<T: Clone + 'static> Op for Buffer<T> {
    type Cfg = usize;
    type State = Vec<T>;
    type In<'a> = (&'a T,);
    type Out = Vec<T>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut usize,
        state: &mut Vec<T>,
        input: (&T,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Vec<T>>> {
        state.push(input.0.clone());
        if state.len() >= *cfg || (!state.is_empty() && ctx.is_last_cycle()) {
            Ok(Tick::Value(std::mem::take(state)))
        } else {
            Ok(Tick::Quiet)
        }
    }
}

/// Combines three streams with a closure — the legacy `trimap`. Ticks when
/// any active input ticks (active/passive is an engine dispatch concern);
/// all three values are read. `Fn`, like [`Join`].
pub struct Join3<A, B, C, D, F>(PhantomData<(A, B, C, D, F)>);

#[op(build = join3, fluent)]
impl<A, B, C, D, F> Op for Join3<A, B, C, D, F>
where
    A: 'static,
    B: 'static,
    C: 'static,
    D: Clone + 'static,
    F: Fn(&A, &B, &C) -> D + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a A, &'a B, &'a C);
    type Out = D;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut F,
        _state: &mut (),
        input: (&A, &B, &C),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<D>> {
        Ok(Tick::Value(cfg(input.0, input.1, input.2)))
    }
}

/// Combines three streams with a *fallible* closure — the legacy
/// `try_trimap`. The `try_` counterpart to [`Join3`]: any returned `Err`
/// propagates to abort the run with context. The closure is `Fn`, like
/// [`Join3`].
pub struct TryJoin3<A, B, C, D, F>(PhantomData<(A, B, C, D, F)>);

#[op(build = try_join3, fluent)]
impl<A, B, C, D, F> Op for TryJoin3<A, B, C, D, F>
where
    A: 'static,
    B: 'static,
    C: 'static,
    D: Clone + 'static,
    F: Fn(&A, &B, &C) -> Result<D> + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a A, &'a B, &'a C);
    type Out = D;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut F,
        _state: &mut (),
        input: (&A, &B, &C),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<D>> {
        Ok(Tick::Value(cfg(input.0, input.1, input.2)?))
    }
}

/// Pairs each value with the current engine time: emits `(time, value)`.
///
/// `no_builder`: a hand-written [`Builder::with_time`](crate::interp::Builder::with_time)
/// seeds the output `(NanoTime, T)` slot from the input's current value, so the
/// interpreted path needs only `T: Clone` (never `T: Default`). The `#[op]`
/// attribute is kept for its `nitro!` forwarders, which seed the value slot
/// with `Default::default()` — so inside `nitro!`/compiled the output type must
/// be `Default` (`(NanoTime, T): Default`, i.e. `T: Default`). That pre-first-tick
/// seed is only ever read by a passive downstream before the first tick; the op
/// itself ticks in lockstep with its source, so the observed values agree across
/// all three engines.
pub struct WithTime<T>(PhantomData<T>);

#[op(build = with_time, no_builder)]
impl<T: Clone + 'static> Op for WithTime<T> {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a T,);
    type Out = (NanoTime, T);
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&T,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<(NanoTime, T)>> {
        Ok(Tick::Value((ctx.time(), input.0.clone())))
    }
}

/// Emits the current engine time whenever the upstream ticks (the upstream's
/// value is ignored). The `ticked_at` of the catalog.
pub struct TickedAt<T>(PhantomData<T>);

#[op(build = ticked_at, fluent)]
impl<T: 'static> Op for TickedAt<T> {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a T,);
    type Out = NanoTime;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        _input: (&T,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<NanoTime>> {
        Ok(Tick::Value(ctx.time()))
    }
}

/// Emits elapsed engine time (`now - start`) whenever the upstream ticks.
pub struct TickedAtElapsed<T>(PhantomData<T>);

#[op(build = ticked_at_elapsed, fluent)]
impl<T: 'static> Op for TickedAtElapsed<T> {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a T,);
    type Out = NanoTime;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        _input: (&T,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<NanoTime>> {
        Ok(Tick::Value(ctx.time() - ctx.start_time()))
    }
}

/// How an [`Ewma`] decays older observations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EwmaDecay {
    /// Fixed smoothing factor `alpha` applied once per tick (count weighting).
    PerTick(f64),
    /// Time decay with the given half-life in nanoseconds: a sample's weight
    /// halves every `half_life` of elapsed engine time, independent of tick
    /// rate. `alpha = 1 - 2^(-Δt / half_life)`.
    HalfLife(f64),
}

/// Pending state for an [`Ewma`] op.
pub struct EwmaState {
    value: f64,
    initialised: bool,
    last_time: Option<NanoTime>,
}

impl Default for EwmaState {
    fn default() -> Self {
        Self {
            value: f64::NAN,
            initialised: false,
            last_time: None,
        }
    }
}

/// Exponentially weighted moving average of an `f64` stream — a
/// representative statistics-adapter operator: stateful, clock-aware
/// (`HalfLife` decays off engine time), and seeded with an explicit
/// `initialised` flag rather than a `value == 0.0` sentinel (so an average
/// that legitimately reaches `0.0` does not re-seed).
pub struct Ewma;

#[op(build = ewma, fluent)]
impl Op for Ewma {
    type Cfg = EwmaDecay;
    type State = EwmaState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut EwmaDecay,
        state: &mut EwmaState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let sample = *input.0;
        if !state.initialised {
            state.value = sample;
            state.initialised = true;
            state.last_time = Some(ctx.time());
            return Ok(Tick::Value(sample));
        }
        let alpha = match *cfg {
            EwmaDecay::PerTick(alpha) => alpha,
            EwmaDecay::HalfLife(half_life) => {
                let now = ctx.time();
                let prev = state
                    .last_time
                    .expect("invariant: last_time set once initialised");
                state.last_time = Some(now);
                if half_life <= 0.0 {
                    1.0
                } else {
                    let dt = f64::from(now - prev);
                    1.0 - (-(dt / half_life) * std::f64::consts::LN_2).exp()
                }
            }
        };
        debug_assert!(
            (0.0..=1.0).contains(&alpha),
            "ewma smoothing factor must be in [0, 1], got {alpha}"
        );
        state.value += alpha * (sample - state.value);
        Ok(Tick::Value(state.value))
    }
}

/// [`Ewma`] with a fixed per-tick smoothing factor — `Cfg` is the bare
/// `alpha` as passed at the call site; the [`EwmaDecay`] policy is built
/// inside `cycle` (an enum wrap, folded after monomorphization). Previously
/// call-site sugar duplicated in the fluent layer and the `nitro!` macro.
pub struct EwmaPerTick;

#[op(build = ewma_per_tick)]
impl Op for EwmaPerTick {
    type Cfg = f64;
    type State = EwmaState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut f64,
        state: &mut EwmaState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let mut decay = EwmaDecay::PerTick(*cfg);
        Ewma::cycle(&mut decay, state, input, ctx)
    }
}

/// [`Ewma`] whose weights decay off engine time — `Cfg` is the half-life as
/// passed at the call site (`Duration`); converted per activation, which is
/// noise next to the `exp()` in the decay itself.
pub struct EwmaHalfLife;

#[op(build = ewma_half_life, fluent)]
impl Op for EwmaHalfLife {
    type Cfg = Duration;
    type State = EwmaState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut Duration,
        state: &mut EwmaState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let mut decay = EwmaDecay::HalfLife(cfg.as_nanos() as f64);
        Ewma::cycle(&mut decay, state, input, ctx)
    }
}

/// Shared ring-buffer state for the rolling-window statistics ops: the most
/// recent `window` samples and their running sum (maintained in O(1) per
/// tick).
pub struct RollingWindowState {
    buffer: VecDeque<f64>,
    sum: f64,
}

impl Default for RollingWindowState {
    fn default() -> Self {
        Self {
            buffer: VecDeque::new(),
            sum: 0.0,
        }
    }
}

impl RollingWindowState {
    /// Push a sample, evicting the oldest once the window is full. Returns the
    /// current window length.
    fn push(&mut self, sample: f64, window: usize) -> usize {
        self.buffer.push_back(sample);
        self.sum += sample;
        if self.buffer.len() > window {
            let oldest = self
                .buffer
                .pop_front()
                .expect("invariant: len > window >= 1 implies non-empty");
            self.sum -= oldest;
        }
        self.buffer.len()
    }
}

/// Rolling sum over the most recent `window` `f64` samples. `Cfg` = window.
pub struct RollingSum;

#[op(build = rolling_sum, fluent)]
impl Op for RollingSum {
    type Cfg = usize;
    type State = RollingWindowState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut usize,
        state: &mut RollingWindowState,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.push(*input.0, (*cfg).max(1));
        Ok(Tick::Value(state.sum))
    }
}

/// Rolling arithmetic mean over the most recent `window` `f64` samples.
pub struct RollingMean;

#[op(build = rolling_mean, fluent)]
impl Op for RollingMean {
    type Cfg = usize;
    type State = RollingWindowState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut usize,
        state: &mut RollingWindowState,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let len = state.push(*input.0, (*cfg).max(1));
        Ok(Tick::Value(state.sum / len as f64))
    }
}

/// Ring-buffer state for the rolling variance / std ops: the most recent
/// `window` samples plus incrementally maintained count-weighted moments
/// (Welford's algorithm with exact removal), so a tick is O(1). Mirrors the
/// legacy `RollingMomentStream` under `Weighting::Count`.
pub struct RollingMomentState {
    buffer: VecDeque<f64>,
    count: u64,
    mean: f64,
    m2: f64,
}

impl Default for RollingMomentState {
    fn default() -> Self {
        Self {
            buffer: VecDeque::new(),
            count: 0,
            mean: 0.0,
            m2: 0.0,
        }
    }
}

impl RollingMomentState {
    /// Fold a new sample into the moments (Welford update).
    fn add(&mut self, x: f64) {
        self.count += 1;
        let mean_old = self.mean;
        self.mean += (x - mean_old) / self.count as f64;
        self.m2 += (x - mean_old) * (x - self.mean);
    }

    /// Exact inverse of [`add`](Self::add): drop a previously added sample so a
    /// sliding window can evict its oldest contribution in O(1). Like any
    /// revert-based scheme this can accumulate floating-point error, so `m2` is
    /// clamped at zero.
    fn remove(&mut self, x: f64) {
        // Removing the final contribution empties the accumulator; reset
        // cleanly rather than dividing by a count at (or below) zero.
        if self.count <= 1 {
            self.count = 0;
            self.mean = 0.0;
            self.m2 = 0.0;
            return;
        }
        let n = self.count as f64;
        let mean_old = (n * self.mean - x) / (n - 1.0);
        self.m2 -= (x - mean_old) * (x - self.mean);
        if self.m2 < 0.0 {
            self.m2 = 0.0;
        }
        self.mean = mean_old;
        self.count -= 1;
    }

    /// Push a sample, evicting the oldest once the window is full.
    fn push(&mut self, sample: f64, window: usize) {
        self.buffer.push_back(sample);
        self.add(sample);
        if self.buffer.len() > window {
            let oldest = self
                .buffer
                .pop_front()
                .expect("invariant: len > window >= 1 implies non-empty");
            self.remove(oldest);
        }
    }

    /// Sample variance (ddof = 1) — the legacy `Weighting::Count` convention.
    /// Yields `0.0` while fewer than two samples are present (rather than NaN).
    fn variance(&self) -> f64 {
        if self.count < 2 {
            return 0.0;
        }
        self.m2 / (self.count as f64 - 1.0)
    }
}

/// Rolling **sample** variance (ddof = 1) over the most recent `window` `f64`
/// samples. Matches the legacy statistics adapter's `Weighting::Count`
/// convention: the divisor is `n - 1`, and the result is `0.0` until at least
/// two samples are in the window. `Cfg` = window.
pub struct RollingVar;

#[op(build = rolling_var, fluent)]
impl Op for RollingVar {
    type Cfg = usize;
    type State = RollingMomentState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut usize,
        state: &mut RollingMomentState,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.push(*input.0, (*cfg).max(1));
        Ok(Tick::Value(state.variance()))
    }
}

/// Rolling **sample** standard deviation over the most recent `window` `f64`
/// samples — the square root of [`RollingVar`] under the same (ddof = 1)
/// convention. Clamped at zero before the square root so a constant window
/// (whose incremental variance can drift a hair negative) yields `0.0`, not
/// `NaN`. `Cfg` = window.
pub struct RollingStd;

#[op(build = rolling_std, fluent)]
impl Op for RollingStd {
    type Cfg = usize;
    type State = RollingMomentState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut usize,
        state: &mut RollingMomentState,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.push(*input.0, (*cfg).max(1));
        Ok(Tick::Value(state.variance().max(0.0).sqrt()))
    }
}

/// Monotonic-deque state for the rolling min & max ops. Holds `(index, value)`
/// candidates kept monotonic in value, so the front is always the window
/// extreme — O(1) amortised per tick. Mirrors the legacy `RollingExtremeStream`
/// (a monotonic deque, not a per-tick window scan, so the tick semantics match
/// the legacy node exactly even though a scan would give the same values).
#[derive(Default)]
pub struct RollingExtremeState {
    deque: VecDeque<(u64, f64)>,
    index: u64,
}

impl RollingExtremeState {
    /// Push a sample and return the current window extreme. `is_min` selects
    /// which back candidates the new sample dominates: for a minimum we drop
    /// candidates `>= sample`, for a maximum `<= sample`.
    fn push(&mut self, sample: f64, window: usize, is_min: bool) -> f64 {
        let window = window.max(1) as u64;
        let i = self.index;
        self.index += 1;

        // Drop back candidates the new sample dominates (they can never again
        // be the extreme while it is in the window).
        while let Some(&(_, back)) = self.deque.back() {
            let dominated = if is_min {
                back >= sample
            } else {
                back <= sample
            };
            if dominated {
                self.deque.pop_back();
            } else {
                break;
            }
        }
        self.deque.push_back((i, sample));

        // Drop the front once it falls outside the last `window` indices. The
        // just-pushed `i` is always in window, so the deque never empties.
        while let Some(&(idx, _)) = self.deque.front() {
            if i - idx >= window {
                self.deque.pop_front();
            } else {
                break;
            }
        }
        self.deque
            .front()
            .expect("invariant: deque holds the current sample")
            .1
    }
}

/// Rolling minimum over the most recent `window` `f64` samples, via a monotonic
/// deque — O(1) amortised per tick. `Cfg` = window.
pub struct RollingMin;

#[op(build = rolling_min, fluent)]
impl Op for RollingMin {
    type Cfg = usize;
    type State = RollingExtremeState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut usize,
        state: &mut RollingExtremeState,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        Ok(Tick::Value(state.push(*input.0, *cfg, true)))
    }
}

/// Rolling maximum over the most recent `window` `f64` samples, via a monotonic
/// deque — O(1) amortised per tick. `Cfg` = window.
pub struct RollingMax;

#[op(build = rolling_max, fluent)]
impl Op for RollingMax {
    type Cfg = usize;
    type State = RollingExtremeState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut usize,
        state: &mut RollingExtremeState,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        Ok(Tick::Value(state.push(*input.0, *cfg, false)))
    }
}

/// Ring-buffer state for the rolling median op — retains the most recent
/// `window` samples and recomputes the median (sort) each tick, matching the
/// legacy recompute-per-tick `WindowStream` (the median has no cheap
/// incremental form here).
#[derive(Default)]
pub struct RollingMedianState {
    buffer: VecDeque<f64>,
}

impl RollingMedianState {
    /// Push a sample (evicting the oldest past `window`) and return the median
    /// of the retained samples. An even-sized window averages the two middle
    /// values, so unit weights reproduce the ordinary median — the legacy
    /// `Weighting::Count` behaviour.
    fn push(&mut self, sample: f64, window: usize) -> f64 {
        self.buffer.push_back(sample);
        while self.buffer.len() > window {
            self.buffer.pop_front();
        }
        let mut sorted: Vec<f64> = self.buffer.iter().copied().collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = sorted.len();
        if n % 2 == 1 {
            sorted[n / 2]
        } else {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
        }
    }
}

/// Rolling median over the most recent `window` `f64` samples. Recomputed per
/// tick (O(window)); an even window averages its two middle values, matching
/// the legacy statistics adapter's count-weighted median. `Cfg` = window.
pub struct RollingMedian;

#[op(build = rolling_median, fluent)]
impl Op for RollingMedian {
    type Cfg = usize;
    type State = RollingMedianState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut usize,
        state: &mut RollingMedianState,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        Ok(Tick::Value(state.push(*input.0, (*cfg).max(1))))
    }
}

/// Incrementally maintained count-weighted moments over the **whole** stream
/// (a cumulative / unbounded window), via Welford's online algorithm — O(1)
/// time and memory, numerically stable (never a naive sum-of-squares). Mirrors
/// the legacy `MomentStream` under `Weighting::Count`: samples never leave, so
/// unlike [`RollingMomentState`] there is no buffer and no removal.
#[derive(Default)]
pub struct CumulativeMomentState {
    count: u64,
    mean: f64,
    m2: f64,
}

impl CumulativeMomentState {
    /// Fold a new sample into the running moments (Welford update).
    fn add(&mut self, x: f64) {
        self.count += 1;
        let mean_old = self.mean;
        self.mean += (x - mean_old) / self.count as f64;
        self.m2 += (x - mean_old) * (x - self.mean);
    }

    /// Sample variance (ddof = 1) — the legacy `Weighting::Count` convention.
    /// Yields `0.0` while fewer than two samples have been seen (rather than
    /// NaN).
    fn variance(&self) -> f64 {
        if self.count < 2 {
            return 0.0;
        }
        self.m2 / (self.count as f64 - 1.0)
    }
}

/// Cumulative arithmetic mean over every sample seen so far — O(1) per tick via
/// Welford (see [`CumulativeMomentState`]). Mirrors the legacy
/// `mean(Window::Unbounded, Weighting::Count)`.
pub struct CumulativeMean;

#[op(build = cumulative_mean, fluent)]
impl Op for CumulativeMean {
    type Cfg = ();
    type State = CumulativeMomentState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut CumulativeMomentState,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.add(*input.0);
        Ok(Tick::Value(state.mean))
    }
}

/// Cumulative **sample** variance (ddof = 1) over every sample seen so far —
/// O(1) per tick via Welford (see [`CumulativeMomentState`]). The divisor is
/// `n - 1`, and the result is `0.0` until at least two samples are present.
/// Mirrors the legacy `variance(Window::Unbounded, Weighting::Count)`.
pub struct CumulativeVar;

#[op(build = cumulative_var, fluent)]
impl Op for CumulativeVar {
    type Cfg = ();
    type State = CumulativeMomentState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut CumulativeMomentState,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.add(*input.0);
        Ok(Tick::Value(state.variance()))
    }
}

/// Cumulative **sample** standard deviation over every sample seen so far — the
/// square root of [`CumulativeVar`] under the same (ddof = 1) convention.
/// Clamped at zero before the square root so a constant stream (whose
/// incremental variance can drift a hair negative) yields `0.0`, not `NaN`.
pub struct CumulativeStd;

#[op(build = cumulative_std, fluent)]
impl Op for CumulativeStd {
    type Cfg = ();
    type State = CumulativeMomentState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut CumulativeMomentState,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.add(*input.0);
        Ok(Tick::Value(state.variance().max(0.0).sqrt()))
    }
}

/// Cumulative sum over every sample seen so far — a running total, O(1) per
/// tick. Mirrors the legacy `sum(Window::Unbounded)` (`CumulativeStream` with
/// `CumulativeStat::Sum`).
pub struct CumulativeSum;

#[op(build = cumulative_sum, fluent)]
impl Op for CumulativeSum {
    type Cfg = ();
    /// The running total (starts at `0.0`, so the first sample seeds it).
    type State = f64;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut f64,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        *state += *input.0;
        Ok(Tick::Value(*state))
    }
}

/// Cumulative minimum over every sample seen so far — a running extreme, O(1)
/// per tick. The `Option` state distinguishes "no sample yet" from a genuine
/// `0.0`, so the first sample seeds the extreme rather than `min(0.0, x)`.
/// Mirrors the legacy `min(Window::Unbounded)`.
pub struct CumulativeMin;

#[op(build = cumulative_min, fluent)]
impl Op for CumulativeMin {
    type Cfg = ();
    /// The running minimum; `None` until the first sample seeds it.
    type State = Option<f64>;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut Option<f64>,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let sample = *input.0;
        let acc = match *state {
            Some(m) => m.min(sample),
            None => sample,
        };
        *state = Some(acc);
        Ok(Tick::Value(acc))
    }
}

/// Cumulative maximum over every sample seen so far — a running extreme, O(1)
/// per tick. `Option` state seeds on the first sample (see [`CumulativeMin`]).
/// Mirrors the legacy `max(Window::Unbounded)`.
pub struct CumulativeMax;

#[op(build = cumulative_max, fluent)]
impl Op for CumulativeMax {
    type Cfg = ();
    /// The running maximum; `None` until the first sample seeds it.
    type State = Option<f64>;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut Option<f64>,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let sample = *input.0;
        let acc = match *state {
            Some(m) => m.max(sample),
            None => sample,
        };
        *state = Some(acc);
        Ok(Tick::Value(acc))
    }
}

/// Retains **every** sample so far and recomputes the median (sort) each tick,
/// mirroring the legacy recompute-per-tick `WindowStream` in its unbounded
/// mode (the median has no cheap incremental form here, so — unlike the other
/// cumulative stats — its memory grows with the stream).
#[derive(Default)]
pub struct CumulativeMedianState {
    buffer: Vec<f64>,
}

impl CumulativeMedianState {
    /// Push a sample (retaining all history) and return the median of every
    /// sample seen so far. An even count averages the two middle values, so
    /// this reproduces the legacy count-weighted median.
    fn push(&mut self, sample: f64) -> f64 {
        self.buffer.push(sample);
        let mut sorted = self.buffer.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = sorted.len();
        if n % 2 == 1 {
            sorted[n / 2]
        } else {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
        }
    }
}

/// Cumulative median over every sample seen so far. Recomputed per tick
/// (O(n log n)); an even count averages the two middle values, matching the
/// legacy count-weighted median. Retains all samples, so its memory grows with
/// the stream. Mirrors the legacy `median(Window::Unbounded, Weighting::Count)`.
pub struct CumulativeMedian;

#[op(build = cumulative_median, fluent)]
impl Op for CumulativeMedian {
    type Cfg = ();
    type State = CumulativeMedianState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut CumulativeMedianState,
        input: (&f64,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        Ok(Tick::Value(state.push(*input.0)))
    }
}

// ── time-windowed rolling statistics (Window::Time, count-weighted) ──────────
//
// Each op keeps the samples currently inside a bounded time window — the last
// `window` nanoseconds of graph time — and computes a statistic over them.
// Eviction semantics, reproduced from the legacy `WindowStream` /
// `RollingMomentStream` `Window::Time` path (`legacy/wingfoil/src/adapters/statistics.rs`):
//
//   * on each tick the current sample is pushed at `now = ctx.time()`, then
//     every front entry whose age `now - t` is **strictly greater** than
//     `window` is evicted — so an entry exactly `window` old is retained (an
//     inclusive trailing boundary);
//   * the just-pushed sample has age 0, so it is always in window and the window
//     is **never empty** (legacy has no empty-window instant — even a zero-width
//     window keeps the current sample); and
//   * `var`/`std` use the sample (ddof = 1) convention — `0.0` until at least two
//     samples are in the window.
//
// All are count-weighted (the ordinary statistic over the samples in the
// window). The time-*weighted* windowed combo (`Weighting::Time` over
// `Window::Time`) is a separate follow-up.

/// True when an entry at time `t` observed from `now` has aged past `window`
/// nanoseconds (the legacy strictly-greater trailing boundary). Time is
/// monotonic, so `now >= t` and the subtraction never underflows.
fn aged_out(now: NanoTime, t: NanoTime, window: u64) -> bool {
    u64::from(now) - u64::from(t) > window
}

/// State wrapper for the time-windowed family: the window converted to engine
/// nanoseconds **once** in `start`, beside whichever accumulator the statistic
/// keeps.
///
/// It exists to let these ops take a `Duration` as their [`Op::Cfg`] — the same
/// type their fluent methods take — which is what puts them in `nitro!` and
/// `compiled()` alongside every other statistic. The compiled path uses the
/// call-site argument tokens verbatim as the config, so an op whose `Cfg` was
/// [`NanoTime`] while its fluent method took a `Duration` could not be spelled
/// in a `nitro!` block at all; that mismatch is why this family was
/// interpreted-only.
///
/// The conversion lands in `start` rather than `cycle` for the reason
/// [`Ticker`] gives: per-cycle it is measurable on dense graphs, and here it
/// would sit on the hot path of every windowed statistic in the graph.
///
/// The window is held *here* rather than as a field on the accumulators
/// because three of the four — [`WindowedTimeWeightedMomentState`],
/// [`TimeWeightedMedianState`] and the moment state — are shared with the
/// **count**-windowed ops, which evict by index and have no window duration to
/// hold. Wrapping keeps that boundary honest: the accumulator stays a pure
/// accumulator, and only the ops that evict by age carry an eviction horizon.
#[derive(Default)]
pub struct TimeWindowed<S> {
    /// The eviction horizon in engine nanoseconds; set by `arm` in `start`.
    window: u64,
    /// The statistic's own accumulator.
    inner: S,
}

impl<S> TimeWindowed<S> {
    /// Convert the configured window to engine nanoseconds. Called from each
    /// op's `start`, so `cycle` reads a `u64` and never converts.
    fn arm(&mut self, window: Duration) {
        self.window = u64::from(NanoTime::from(window));
    }
}

/// Ring-buffer state for the time-windowed sum: the `(time, value)` samples
/// currently inside the window and their running total, maintained
/// incrementally (add on push, subtract on evict) rather than rescanned.
#[derive(Default)]
pub struct TimeWindowSumState {
    buffer: VecDeque<(NanoTime, f64)>,
    sum: f64,
}

impl TimeWindowSumState {
    /// Push `sample` at `now`, evict aged entries, and return the running sum
    /// over what remains.
    fn push(&mut self, now: NanoTime, sample: f64, window: u64) -> f64 {
        self.buffer.push_back((now, sample));
        self.sum += sample;
        while let Some(&(t, v)) = self.buffer.front() {
            if aged_out(now, t, window) {
                self.sum -= v;
                self.buffer.pop_front();
            } else {
                break;
            }
        }
        self.sum
    }
}

/// Sum over a bounded time window of the last `window` (a [`Duration`], converted
/// to engine time in `start`) of graph time — O(1) per tick. Mirrors the legacy `sum(Window::Time(_))`.
pub struct TimeWindowedSum;

#[op(build = time_windowed_sum, fluent)]
impl Op for TimeWindowedSum {
    type Cfg = Duration;
    type State = TimeWindowed<TimeWindowSumState>;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWindowSumState>,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let out = state.inner.push(ctx.time(), *input.0, state.window);
        Ok(Tick::Value(out))
    }

    fn start(
        cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWindowSumState>,
        _ctx: &mut Ctx<'_>,
    ) -> Result<()> {
        state.arm(*cfg);
        Ok(())
    }
}

/// Ring-buffer state for the time-windowed mean / variance / std: the
/// `(time, value)` samples in the window plus incrementally maintained
/// count-weighted moments (Welford's algorithm with exact removal), so a tick
/// is O(1) amortised. Mirrors the legacy `RollingMomentStream` under
/// `Weighting::Count` with a `Window::Time` eviction rule.
#[derive(Default)]
pub struct TimeWindowMomentState {
    buffer: VecDeque<(NanoTime, f64)>,
    count: u64,
    mean: f64,
    m2: f64,
}

impl TimeWindowMomentState {
    /// Fold a new sample into the moments (Welford update).
    fn add(&mut self, x: f64) {
        self.count += 1;
        let mean_old = self.mean;
        self.mean += (x - mean_old) / self.count as f64;
        self.m2 += (x - mean_old) * (x - self.mean);
    }

    /// Exact inverse of [`add`](Self::add): drop a previously added sample as it
    /// leaves the window, in O(1). `m2` is clamped at zero against
    /// revert-scheme floating-point drift.
    fn remove(&mut self, x: f64) {
        if self.count <= 1 {
            self.count = 0;
            self.mean = 0.0;
            self.m2 = 0.0;
            return;
        }
        let n = self.count as f64;
        let mean_old = (n * self.mean - x) / (n - 1.0);
        self.m2 -= (x - mean_old) * (x - self.mean);
        if self.m2 < 0.0 {
            self.m2 = 0.0;
        }
        self.mean = mean_old;
        self.count -= 1;
    }

    /// Push `sample` at `now` and evict every aged entry, removing each from the
    /// moments as it leaves.
    fn push(&mut self, now: NanoTime, sample: f64, window: u64) {
        self.buffer.push_back((now, sample));
        self.add(sample);
        while let Some(&(t, v)) = self.buffer.front() {
            if aged_out(now, t, window) {
                self.buffer.pop_front();
                self.remove(v);
            } else {
                break;
            }
        }
    }

    /// Sample variance (ddof = 1) — `0.0` while fewer than two samples are in
    /// the window.
    fn variance(&self) -> f64 {
        if self.count < 2 {
            return 0.0;
        }
        self.m2 / (self.count as f64 - 1.0)
    }
}

/// Arithmetic mean over a bounded time window — O(1) amortised per tick.
/// Mirrors the legacy `mean(Window::Time(_), Weighting::Count)`.
pub struct TimeWindowedMean;

#[op(build = time_windowed_mean, fluent)]
impl Op for TimeWindowedMean {
    type Cfg = Duration;
    type State = TimeWindowed<TimeWindowMomentState>;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWindowMomentState>,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.inner.push(ctx.time(), *input.0, state.window);
        // The current sample is always in window, so `count >= 1` and `mean`
        // holds the window mean (equal to the sample on the first tick).
        Ok(Tick::Value(state.inner.mean))
    }

    fn start(
        cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWindowMomentState>,
        _ctx: &mut Ctx<'_>,
    ) -> Result<()> {
        state.arm(*cfg);
        Ok(())
    }
}

/// **Sample** variance (ddof = 1) over a bounded time window — O(1) amortised
/// per tick. Mirrors the legacy `variance(Window::Time(_), Weighting::Count)`.
pub struct TimeWindowedVar;

#[op(build = time_windowed_var, fluent)]
impl Op for TimeWindowedVar {
    type Cfg = Duration;
    type State = TimeWindowed<TimeWindowMomentState>;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWindowMomentState>,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.inner.push(ctx.time(), *input.0, state.window);
        Ok(Tick::Value(state.inner.variance()))
    }

    fn start(
        cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWindowMomentState>,
        _ctx: &mut Ctx<'_>,
    ) -> Result<()> {
        state.arm(*cfg);
        Ok(())
    }
}

/// **Sample** standard deviation over a bounded time window — the square root of
/// [`TimeWindowedVar`], clamped at zero before the root so a constant window
/// yields `0.0`, not `NaN`. Mirrors the legacy `std(Window::Time(_),
/// Weighting::Count)`.
pub struct TimeWindowedStd;

#[op(build = time_windowed_std, fluent)]
impl Op for TimeWindowedStd {
    type Cfg = Duration;
    type State = TimeWindowed<TimeWindowMomentState>;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWindowMomentState>,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.inner.push(ctx.time(), *input.0, state.window);
        Ok(Tick::Value(state.inner.variance().max(0.0).sqrt()))
    }

    fn start(
        cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWindowMomentState>,
        _ctx: &mut Ctx<'_>,
    ) -> Result<()> {
        state.arm(*cfg);
        Ok(())
    }
}

/// Monotonic-deque state for the time-windowed min & max. Holds `(time, value)`
/// candidates kept monotonic in value and increasing in time front-to-back, so
/// the front is always the window extreme — O(1) amortised per tick. Unlike the
/// legacy time-windowed `WindowStream` (a per-tick scan), this uses the same
/// monotonic-deque trick as the legacy *count*-windowed `RollingExtremeStream`,
/// front-evicting by age instead of by index; the emitted values are identical.
#[derive(Default)]
pub struct TimeWindowExtremeState {
    deque: VecDeque<(NanoTime, f64)>,
}

impl TimeWindowExtremeState {
    /// Push `sample` at `now` and return the window extreme. `is_min` selects
    /// which back candidates the new sample dominates: for a minimum we drop
    /// candidates `>= sample`, for a maximum `<= sample`.
    fn push(&mut self, now: NanoTime, sample: f64, window: u64, is_min: bool) -> f64 {
        // Drop back candidates the new sample dominates (they can never again be
        // the extreme while it is in the window).
        while let Some(&(_, back)) = self.deque.back() {
            let dominated = if is_min {
                back >= sample
            } else {
                back <= sample
            };
            if dominated {
                self.deque.pop_back();
            } else {
                break;
            }
        }
        self.deque.push_back((now, sample));

        // Drop the front while it has aged out. The just-pushed sample has age
        // 0, so the deque never empties.
        while let Some(&(t, _)) = self.deque.front() {
            if aged_out(now, t, window) {
                self.deque.pop_front();
            } else {
                break;
            }
        }
        self.deque
            .front()
            .expect("invariant: just-pushed sample is within the window")
            .1
    }
}

/// Minimum over a bounded time window, via a monotonic deque — O(1) amortised
/// per tick. Mirrors the legacy `min(Window::Time(_))`.
pub struct TimeWindowedMin;

#[op(build = time_windowed_min, fluent)]
impl Op for TimeWindowedMin {
    type Cfg = Duration;
    type State = TimeWindowed<TimeWindowExtremeState>;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWindowExtremeState>,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let out = state.inner.push(ctx.time(), *input.0, state.window, true);
        Ok(Tick::Value(out))
    }

    fn start(
        cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWindowExtremeState>,
        _ctx: &mut Ctx<'_>,
    ) -> Result<()> {
        state.arm(*cfg);
        Ok(())
    }
}

/// Maximum over a bounded time window, via a monotonic deque — O(1) amortised
/// per tick. Mirrors the legacy `max(Window::Time(_))`.
pub struct TimeWindowedMax;

#[op(build = time_windowed_max, fluent)]
impl Op for TimeWindowedMax {
    type Cfg = Duration;
    type State = TimeWindowed<TimeWindowExtremeState>;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWindowExtremeState>,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let out = state.inner.push(ctx.time(), *input.0, state.window, false);
        Ok(Tick::Value(out))
    }

    fn start(
        cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWindowExtremeState>,
        _ctx: &mut Ctx<'_>,
    ) -> Result<()> {
        state.arm(*cfg);
        Ok(())
    }
}

/// Ring-buffer state for the time-windowed median — retains the `(time, value)`
/// samples in the window and recomputes the median (sort) each tick, matching
/// the legacy recompute-per-tick `WindowStream` (the median has no cheap
/// incremental form here).
#[derive(Default)]
pub struct TimeWindowMedianState {
    buffer: VecDeque<(NanoTime, f64)>,
}

impl TimeWindowMedianState {
    /// Push `sample` at `now`, evict aged entries, and return the median of the
    /// retained samples. An even count averages the two middle values, so this
    /// reproduces the legacy count-weighted median.
    fn push(&mut self, now: NanoTime, sample: f64, window: u64) -> f64 {
        self.buffer.push_back((now, sample));
        while let Some(&(t, _)) = self.buffer.front() {
            if aged_out(now, t, window) {
                self.buffer.pop_front();
            } else {
                break;
            }
        }
        let mut sorted: Vec<f64> = self.buffer.iter().map(|&(_, v)| v).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = sorted.len();
        if n % 2 == 1 {
            sorted[n / 2]
        } else {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
        }
    }
}

/// Median over a bounded time window. Recomputed per tick (O(w log w) over the
/// `w` retained samples); an even count averages the two middle values,
/// matching the legacy count-weighted median. Mirrors the legacy
/// `median(Window::Time(_), Weighting::Count)`.
pub struct TimeWindowedMedian;

#[op(build = time_windowed_median, fluent)]
impl Op for TimeWindowedMedian {
    type Cfg = Duration;
    type State = TimeWindowed<TimeWindowMedianState>;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWindowMedianState>,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let out = state.inner.push(ctx.time(), *input.0, state.window);
        Ok(Tick::Value(out))
    }

    fn start(
        cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWindowMedianState>,
        _ctx: &mut Ctx<'_>,
    ) -> Result<()> {
        state.arm(*cfg);
        Ok(())
    }
}

// ── time-weighted moments (Weighting::Time) ──────────────────────────────────
//
// The time-*weighted* twin of the count-weighted moment ops above (cumulative,
// count-windowed, and time-windowed mean/var/std): each sample is weighted by
// how long it was in effect — the elapsed Δt until its successor, read from
// engine time via `ctx.time()` — rather than by a unit count. Ported verbatim
// from the legacy `MomentStream` / `RollingMomentStream` under
// `Weighting::Time` (`legacy/wingfoil/src/adapters/statistics.rs`). Semantics
// reproduced:
//
//   * on each tick the *previous* sample is credited with the elapsed Δt before
//     the new sample takes over — the correct treatment of a left-continuous
//     step signal, so the newest sample contributes nothing until the next tick
//     advances the clock (a value in effect for twice as long counts twice);
//   * a windowed sample's committed interval is reverted when it leaves the
//     window (an exact `remove` inverse keeps a tick O(1));
//   * mean seeds to the current sample until any weight has accumulated (rather
//     than emitting NaN); variance is the time-weighted *population* form
//     (`m2 / w_sum`, no ddof correction — reliability-weighted variance has no
//     clean ddof), `0.0` until weight is present; std clamps at zero.

/// Incremental weighted mean and variance via West's algorithm (the weighted
/// generalisation of Welford's — numerically stable, single pass), with an
/// exact `remove` inverse so a sliding window can evict a contribution in O(1).
/// Ported verbatim from the legacy statistics adapter's `WeightedMoments`.
#[derive(Default)]
struct WeightedMoments {
    w_sum: f64,
    count: u64,
    mean: f64,
    m2: f64,
}

impl WeightedMoments {
    /// Fold a `(value, weight)` sample into the moments. Zero-duration
    /// (simultaneous ticks) or non-positive weights add nothing.
    fn push(&mut self, x: f64, weight: f64) {
        if weight <= 0.0 {
            return;
        }
        self.w_sum += weight;
        self.count += 1;
        let mean_old = self.mean;
        self.mean += (weight / self.w_sum) * (x - mean_old);
        self.m2 += weight * (x - mean_old) * (x - self.mean);
    }

    /// Exact inverse of [`push`](Self::push): drop a `(value, weight)` previously
    /// pushed, so a sliding window can evict its oldest contribution in O(1).
    /// Like any revert-based scheme this can accumulate floating-point error, so
    /// `m2` is clamped at zero.
    fn remove(&mut self, x: f64, weight: f64) {
        if weight <= 0.0 {
            return;
        }
        let w_new = self.w_sum - weight;
        // Removing the final contribution empties the accumulator; reset cleanly
        // rather than dividing by a weight at (or below) zero.
        if self.count <= 1 || w_new <= 0.0 {
            *self = Self::default();
            return;
        }
        self.count -= 1;
        // Recover the pre-push mean, then invert the M2 update with it.
        let mean_old = (self.w_sum * self.mean - weight * x) / w_new;
        self.m2 -= weight * (x - mean_old) * (x - self.mean);
        if self.m2 < 0.0 {
            self.m2 = 0.0;
        }
        self.mean = mean_old;
        self.w_sum = w_new;
    }

    fn is_empty(&self) -> bool {
        self.w_sum <= 0.0
    }

    fn mean(&self) -> f64 {
        self.mean
    }

    /// Time-weighted (population) variance — `m2 / w_sum`, `0.0` until weight is
    /// present. Reliability-weighted variance has no clean ddof correction, so
    /// this is the population form (matching the legacy `Weighting::Time`).
    fn variance(&self) -> f64 {
        if self.w_sum <= 0.0 {
            return 0.0;
        }
        self.m2 / self.w_sum
    }
}

/// State for the cumulative time-weighted moment ops: the running
/// [`WeightedMoments`] plus the previous sample and the time it was last seen,
/// so each tick credits the previous value with the elapsed Δt before the new
/// sample takes over. Mirrors the legacy `MomentStream` under
/// `Weighting::Time`.
#[derive(Default)]
pub struct CumulativeTimeWeightedMomentState {
    moments: WeightedMoments,
    last_time: Option<NanoTime>,
    prev_value: f64,
}

impl CumulativeTimeWeightedMomentState {
    /// Credit the previous sample with the Δt since it arrived, then record the
    /// new sample as current. (`prev_value` is only read once `last_time` is
    /// set — i.e. after a prior tick wrote it — so its initial value is unused.)
    fn push(&mut self, now: NanoTime, sample: f64) {
        if let Some(prev_t) = self.last_time {
            let dt = f64::from(now - prev_t);
            self.moments.push(self.prev_value, dt);
        }
        self.prev_value = sample;
        self.last_time = Some(now);
    }

    /// Time-weighted mean — seeds to `sample` until any weight has accumulated.
    fn mean(&self, sample: f64) -> f64 {
        if self.moments.is_empty() {
            sample
        } else {
            self.moments.mean()
        }
    }

    fn variance(&self) -> f64 {
        self.moments.variance()
    }
}

/// Cumulative time-weighted mean over every sample seen so far — each sample
/// weighted by how long it was in effect (Δt from the graph clock). The most
/// recent sample only starts contributing once the next tick advances the
/// clock. Mirrors the legacy `mean(Window::Unbounded, Weighting::Time)`.
pub struct CumulativeMeanTimeWeighted;

#[op(build = cumulative_mean_time_weighted, fluent)]
impl Op for CumulativeMeanTimeWeighted {
    type Cfg = ();
    type State = CumulativeTimeWeightedMomentState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut CumulativeTimeWeightedMomentState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let sample = *input.0;
        state.push(ctx.time(), sample);
        Ok(Tick::Value(state.mean(sample)))
    }
}

/// Cumulative time-weighted **population** variance over every sample seen so
/// far — `m2 / w_sum` (no ddof correction), `0.0` until weight is present.
/// Mirrors the legacy `variance(Window::Unbounded, Weighting::Time)`.
pub struct CumulativeVarTimeWeighted;

#[op(build = cumulative_var_time_weighted, fluent)]
impl Op for CumulativeVarTimeWeighted {
    type Cfg = ();
    type State = CumulativeTimeWeightedMomentState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut CumulativeTimeWeightedMomentState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.push(ctx.time(), *input.0);
        Ok(Tick::Value(state.variance()))
    }
}

/// Cumulative time-weighted standard deviation over every sample seen so far —
/// the square root of [`CumulativeVarTimeWeighted`], clamped at zero before the
/// root so a constant stream yields `0.0`, not `NaN`. Mirrors the legacy
/// `std(Window::Unbounded, Weighting::Time)`.
pub struct CumulativeStdTimeWeighted;

#[op(build = cumulative_std_time_weighted, fluent)]
impl Op for CumulativeStdTimeWeighted {
    type Cfg = ();
    type State = CumulativeTimeWeightedMomentState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut CumulativeTimeWeightedMomentState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.push(ctx.time(), *input.0);
        Ok(Tick::Value(state.variance().max(0.0).sqrt()))
    }
}

/// State shared by the count- and time-windowed time-weighted moment ops: the
/// `(value, time)` samples currently in the window plus the incrementally
/// maintained [`WeightedMoments`]. Each sample's weight is the Δt until its
/// successor, committed when that successor arrives (so the newest sample
/// contributes nothing until the next tick) and reverted when the sample leaves
/// the window. Mirrors the legacy `RollingMomentStream` under
/// `Weighting::Time`; only the eviction rule differs between the count and time
/// windows.
#[derive(Default)]
pub struct WindowedTimeWeightedMomentState {
    buffer: VecDeque<(f64, NanoTime)>,
    moments: WeightedMoments,
}

impl WindowedTimeWeightedMomentState {
    /// Commit the interval the previous sample held (crediting it with the Δt to
    /// `now`), then append the new sample. The new sample opens an interval that
    /// only commits once its successor arrives.
    fn open(&mut self, now: NanoTime, sample: f64) {
        if let Some(&(prev_v, prev_t)) = self.buffer.back() {
            self.moments.push(prev_v, f64::from(now - prev_t));
        }
        self.buffer.push_back((sample, now));
    }

    /// Evict the front sample, reverting its committed interval (the gap to the
    /// new front). An empty buffer after the pop means the sample never had a
    /// committed interval (it was the lone/newest sample), so nothing to revert.
    fn evict_front(&mut self) {
        let (old_v, old_t) = self
            .buffer
            .pop_front()
            .expect("invariant: caller checked the buffer is non-empty");
        if let Some(&(_, next_t)) = self.buffer.front() {
            self.moments.remove(old_v, f64::from(next_t - old_t));
        }
    }

    /// Push a sample under a **count** window (clamped to at least one),
    /// evicting once more than `window` samples are retained.
    fn push_count(&mut self, now: NanoTime, sample: f64, window: usize) {
        let window = window.max(1);
        self.open(now, sample);
        while self.buffer.len() > window {
            self.evict_front();
        }
    }

    /// Push a sample under a **time** window, evicting every front sample aged
    /// strictly past `window` nanoseconds (the just-pushed sample has age 0, so
    /// the buffer never empties).
    fn push_time(&mut self, now: NanoTime, sample: f64, window: u64) {
        self.open(now, sample);
        while self
            .buffer
            .front()
            .is_some_and(|&(_, t)| aged_out(now, t, window))
        {
            self.evict_front();
        }
    }

    /// Time-weighted mean — seeds to `sample` until any weight has accumulated.
    fn mean(&self, sample: f64) -> f64 {
        if self.moments.is_empty() {
            sample
        } else {
            self.moments.mean()
        }
    }

    fn variance(&self) -> f64 {
        self.moments.variance()
    }
}

/// Time-weighted mean over the most recent `window` samples (a count window).
/// Mirrors the legacy `mean(Window::Count(_), Weighting::Time)`.
pub struct RollingMeanTimeWeighted;

#[op(build = rolling_mean_time_weighted, fluent)]
impl Op for RollingMeanTimeWeighted {
    type Cfg = usize;
    type State = WindowedTimeWeightedMomentState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut usize,
        state: &mut WindowedTimeWeightedMomentState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let sample = *input.0;
        state.push_count(ctx.time(), sample, *cfg);
        Ok(Tick::Value(state.mean(sample)))
    }
}

/// Time-weighted **population** variance over the most recent `window` samples
/// (a count window). Mirrors the legacy `variance(Window::Count(_),
/// Weighting::Time)`.
pub struct RollingVarTimeWeighted;

#[op(build = rolling_var_time_weighted, fluent)]
impl Op for RollingVarTimeWeighted {
    type Cfg = usize;
    type State = WindowedTimeWeightedMomentState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut usize,
        state: &mut WindowedTimeWeightedMomentState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.push_count(ctx.time(), *input.0, *cfg);
        Ok(Tick::Value(state.variance()))
    }
}

/// Time-weighted standard deviation over the most recent `window` samples (a
/// count window) — the square root of [`RollingVarTimeWeighted`], clamped at
/// zero. Mirrors the legacy `std(Window::Count(_), Weighting::Time)`.
pub struct RollingStdTimeWeighted;

#[op(build = rolling_std_time_weighted, fluent)]
impl Op for RollingStdTimeWeighted {
    type Cfg = usize;
    type State = WindowedTimeWeightedMomentState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut usize,
        state: &mut WindowedTimeWeightedMomentState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.push_count(ctx.time(), *input.0, *cfg);
        Ok(Tick::Value(state.variance().max(0.0).sqrt()))
    }
}

/// Time-weighted mean over a bounded time window of the last `window` (a
/// [`Duration`], converted to engine time in `start`) of graph time. Mirrors the legacy
/// `mean(Window::Time(_), Weighting::Time)`.
pub struct TimeWindowedMeanTimeWeighted;

#[op(build = time_windowed_mean_time_weighted, fluent)]
impl Op for TimeWindowedMeanTimeWeighted {
    type Cfg = Duration;
    type State = TimeWindowed<WindowedTimeWeightedMomentState>;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut Duration,
        state: &mut TimeWindowed<WindowedTimeWeightedMomentState>,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let sample = *input.0;
        state.inner.push_time(ctx.time(), sample, state.window);
        Ok(Tick::Value(state.inner.mean(sample)))
    }

    fn start(
        cfg: &mut Duration,
        state: &mut TimeWindowed<WindowedTimeWeightedMomentState>,
        _ctx: &mut Ctx<'_>,
    ) -> Result<()> {
        state.arm(*cfg);
        Ok(())
    }
}

/// Time-weighted **population** variance over a bounded time window. Mirrors the
/// legacy `variance(Window::Time(_), Weighting::Time)`.
pub struct TimeWindowedVarTimeWeighted;

#[op(build = time_windowed_var_time_weighted, fluent)]
impl Op for TimeWindowedVarTimeWeighted {
    type Cfg = Duration;
    type State = TimeWindowed<WindowedTimeWeightedMomentState>;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut Duration,
        state: &mut TimeWindowed<WindowedTimeWeightedMomentState>,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.inner.push_time(ctx.time(), *input.0, state.window);
        Ok(Tick::Value(state.inner.variance()))
    }

    fn start(
        cfg: &mut Duration,
        state: &mut TimeWindowed<WindowedTimeWeightedMomentState>,
        _ctx: &mut Ctx<'_>,
    ) -> Result<()> {
        state.arm(*cfg);
        Ok(())
    }
}

/// Time-weighted standard deviation over a bounded time window — the square root
/// of [`TimeWindowedVarTimeWeighted`], clamped at zero. Mirrors the legacy
/// `std(Window::Time(_), Weighting::Time)`.
pub struct TimeWindowedStdTimeWeighted;

#[op(build = time_windowed_std_time_weighted, fluent)]
impl Op for TimeWindowedStdTimeWeighted {
    type Cfg = Duration;
    type State = TimeWindowed<WindowedTimeWeightedMomentState>;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut Duration,
        state: &mut TimeWindowed<WindowedTimeWeightedMomentState>,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.inner.push_time(ctx.time(), *input.0, state.window);
        Ok(Tick::Value(state.inner.variance().max(0.0).sqrt()))
    }

    fn start(
        cfg: &mut Duration,
        state: &mut TimeWindowed<WindowedTimeWeightedMomentState>,
        _ctx: &mut Ctx<'_>,
    ) -> Result<()> {
        state.arm(*cfg);
        Ok(())
    }
}

// ── time-weighted median (Weighting::Time) ───────────────────────────────────
//
// The time-*weighted* twin of the count-weighted median ops (cumulative,
// count-windowed, and time-windowed). Unlike the moment ops above it is **not**
// an incremental West's-algorithm accumulator: the median has no cheap
// incremental form, so — like the legacy `WindowStream::weighted_median` — it
// retains the `(value, time)` samples in the window and recomputes over them
// each tick. Ported verbatim from `legacy/wingfoil/src/adapters/statistics.rs`
// (`WindowStream` + `for_each_weight` + `weighted_median`, `Weighting::Time`).
// Semantics reproduced:
//
//   * each retained sample is weighted by the interval it was in effect — the
//     gap to its successor in the buffer, and the most recent sample by the gap
//     to `now` (which is zero, since it was just pushed at `now`, so the newest
//     sample carries no weight until the next tick advances the clock);
//   * zero-weight samples (a Δt of 0, e.g. the newest sample or simultaneous
//     ticks) are dropped; if *every* retained sample has zero weight the latest
//     value is returned;
//   * the samples are sorted by value and the result is the value at which
//     cumulative weight first crosses half the total; a crossing landing exactly
//     on a boundary averages the two straddling values (so unit-length intervals
//     reproduce the even-count average, matching the count-weighted median);
//   * the leading edge uses only retained samples, so a value in effect *before*
//     the window opened is not carried in (the count/time eviction happens first).

/// Recompute-per-tick buffer for the time-weighted median: the `(value, time)`
/// samples currently in the window. Each tick pushes the new sample, evicts per
/// the window rule, then recomputes the weighted median over what remains — the
/// same `VecDeque<(f64, NanoTime)>` and algorithm as the legacy `WindowStream`
/// under `Weighting::Time`; only the eviction rule differs between the three
/// windows.
#[derive(Default)]
pub struct TimeWeightedMedianState {
    buffer: VecDeque<(f64, NanoTime)>,
}

impl TimeWeightedMedianState {
    /// Time-weighted median of the retained samples: weight each by the gap to
    /// its successor (the newest by the gap to `now`, i.e. zero), drop zero
    /// weights, then return the value at which cumulative weight crosses half the
    /// total (averaging the two straddling values on an exact boundary). Ported
    /// verbatim from the legacy `WindowStream::weighted_median`.
    fn weighted_median(&self, now: NanoTime) -> f64 {
        let n = self.buffer.len();
        let mut pairs: Vec<(f64, f64)> = Vec::with_capacity(n);
        for i in 0..n {
            let (v, t) = self.buffer[i];
            let next_t = if i + 1 < n { self.buffer[i + 1].1 } else { now };
            let w = f64::from(next_t - t);
            // Drop zero-weight samples (a time-weighted newest sample with Δt 0).
            if w > 0.0 {
                pairs.push((v, w));
            }
        }
        if pairs.is_empty() {
            // Every retained sample had zero weight; fall back to the latest.
            return self.buffer.back().expect("invariant: buffer non-empty").0;
        }
        pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let half = pairs.iter().map(|&(_, w)| w).sum::<f64>() / 2.0;
        let mut cumulative = 0.0;
        for (i, &(v, w)) in pairs.iter().enumerate() {
            cumulative += w;
            if cumulative > half {
                return v;
            }
            if cumulative == half {
                // Crossing lands exactly on a boundary: average with the next
                // value if there is one (the even-count unit-weight median).
                return match pairs.get(i + 1) {
                    Some(&(next, _)) => (v + next) / 2.0,
                    None => v,
                };
            }
        }
        // Only reachable through floating-point rounding; the last value wins.
        pairs.last().expect("invariant: pairs non-empty").0
    }

    /// Push a sample under an **unbounded** (cumulative) window — every sample is
    /// retained, so memory grows with the stream — and return the median.
    fn push_cumulative(&mut self, now: NanoTime, sample: f64) -> f64 {
        self.buffer.push_back((sample, now));
        self.weighted_median(now)
    }

    /// Push a sample under a **count** window (clamped to at least one), evicting
    /// the oldest once more than `window` samples are retained, and return the
    /// median.
    fn push_count(&mut self, now: NanoTime, sample: f64, window: usize) -> f64 {
        let window = window.max(1);
        self.buffer.push_back((sample, now));
        while self.buffer.len() > window {
            self.buffer.pop_front();
        }
        self.weighted_median(now)
    }

    /// Push a sample under a **time** window, evicting every front sample aged
    /// strictly past `window` nanoseconds (the just-pushed sample has age 0, so
    /// the buffer never empties), and return the median.
    fn push_time(&mut self, now: NanoTime, sample: f64, window: u64) -> f64 {
        self.buffer.push_back((sample, now));
        while self
            .buffer
            .front()
            .is_some_and(|&(_, t)| aged_out(now, t, window))
        {
            self.buffer.pop_front();
        }
        self.weighted_median(now)
    }
}

/// Cumulative time-weighted median over every sample seen so far — each sample
/// weighted by how long it was in effect (Δt from the graph clock). Recomputed
/// per tick (O(n log n)); retains all samples, so its memory grows with the
/// stream. Mirrors the legacy `median(Window::Unbounded, Weighting::Time)`.
pub struct CumulativeMedianTimeWeighted;

#[op(build = cumulative_median_time_weighted, fluent)]
impl Op for CumulativeMedianTimeWeighted {
    type Cfg = ();
    type State = TimeWeightedMedianState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut TimeWeightedMedianState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        Ok(Tick::Value(state.push_cumulative(ctx.time(), *input.0)))
    }
}

/// Time-weighted median over the most recent `window` samples (a count window),
/// each weighted by how long it was in effect. Recomputed per tick (O(w log w)).
/// Mirrors the legacy `median(Window::Count(_), Weighting::Time)`.
pub struct RollingMedianTimeWeighted;

#[op(build = rolling_median_time_weighted, fluent)]
impl Op for RollingMedianTimeWeighted {
    type Cfg = usize;
    type State = TimeWeightedMedianState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut usize,
        state: &mut TimeWeightedMedianState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        Ok(Tick::Value(state.push_count(ctx.time(), *input.0, *cfg)))
    }
}

/// Time-weighted median over a bounded time window — the samples seen in the last
/// `window` of graph time, each weighted by how long it was in effect. Recomputed
/// per tick (O(w log w) over the `w` retained samples). Mirrors the legacy
/// `median(Window::Time(_), Weighting::Time)`.
pub struct TimeWindowedMedianTimeWeighted;

#[op(build = time_windowed_median_time_weighted, fluent)]
impl Op for TimeWindowedMedianTimeWeighted {
    type Cfg = Duration;
    type State = TimeWindowed<TimeWeightedMedianState>;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWeightedMedianState>,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        Ok(Tick::Value(state.inner.push_time(
            ctx.time(),
            *input.0,
            state.window,
        )))
    }

    fn start(
        cfg: &mut Duration,
        state: &mut TimeWindowed<TimeWeightedMedianState>,
        _ctx: &mut Ctx<'_>,
    ) -> Result<()> {
        state.arm(*cfg);
        Ok(())
    }
}

/// Emits its source value when the condition stream's current value is true.
/// Condition ticks resample the held source value after the source has ticked
/// at least once. Before the first source tick, the filter stays quiet.
pub struct Filter<T>(PhantomData<T>);

#[op(build = filter, fluent)]
impl<T: Clone + 'static> Op for Filter<T> {
    type Cfg = ();
    type State = bool;
    type In<'a> = ((&'a T, bool), (&'a bool, bool));
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        source_seen: &mut bool,
        input: ((&T, bool), (&bool, bool)),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        let ((source, source_ticked), (condition, _)) = input;
        if source_ticked {
            *source_seen = true;
        }
        if *source_seen && *condition {
            Ok(Tick::Value(source.clone()))
        } else {
            Ok(Tick::Quiet)
        }
    }
}

/// Emits its source value on the ticks a **predicate** accepts, and stays
/// [`Quiet`](Tick::Quiet) otherwise.
///
/// The predicate twin of [`Filter`], which gates on a separate
/// `Stream<bool>`. Reach for whichever matches the shape of the condition:
///
/// * `filter_value` when the test is a pure function of *this* value —
///   `price.filter_value(|p| *p > 100.0)`. One node, no second stream.
/// * [`Filter`] when the condition is itself a stream — derived from other
///   inputs, shared between several gates, or ticking on its own schedule.
///
/// Neither is sugar for the other: a `Stream<bool>` carries a value that can
/// be *stale* relative to this tick (that is the point — it is a latch),
/// where a predicate is always evaluated against the value being gated.
///
/// The closure is `Fn`, not `FnMut` (see [`Map`] for why); a predicate that
/// needs memory belongs in [`Fold`]/[`Scan`] feeding a [`Filter`].
pub struct FilterValue<T, F>(PhantomData<(T, F)>);

#[op(build = filter_value, fluent)]
impl<T, F> Op for FilterValue<T, F>
where
    T: Clone + 'static,
    F: Fn(&T) -> bool + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a T,);
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut F, _state: &mut (), input: (&T,), _ctx: &mut Ctx<'_>) -> Result<Tick<T>> {
        if cfg(input.0) {
            Ok(Tick::Value(input.0.clone()))
        } else {
            Ok(Tick::Quiet)
        }
    }
}

/// Folds inputs into an accumulator; emits the accumulator after each fold.
/// The accumulator is the state — initialised by the engine, so `fold` can
/// start from any value, not just `Default`. The closure is `Fn` (see
/// [`Map`]); all mutation goes through the `&mut B` accumulator argument,
/// which the engine owns.
///
/// [`Scan`] is the same computation with the closure *returning* the new
/// accumulator instead of mutating it in place — pick by accumulator shape,
/// see [`Scan`]'s docs.
pub struct Fold<A, B, F>(PhantomData<(A, B, F)>);

#[op(build = fold, fluent, init_arg)]
impl<A, B, F> Op for Fold<A, B, F>
where
    A: 'static,
    B: Clone + 'static,
    F: Fn(&mut B, &A) + 'static,
{
    type Cfg = F;
    type State = B;
    type In<'a> = (&'a A,);
    type Out = B;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut F, state: &mut B, input: (&A,), _ctx: &mut Ctx<'_>) -> Result<Tick<B>> {
        cfg(state, input.0);
        Ok(Tick::Value(state.clone()))
    }
}

/// Folds inputs into an accumulator, with the closure **returning** the new
/// accumulator; emits it after each fold. The value-returning twin of
/// [`Fold`], and identical to it in semantics, cost model and tick behaviour
/// — the two differ only in how the closure is spelled:
///
/// ```text
/// fold(0u64, |acc, v| *acc += v)        // Fn(&mut B, &A)      — mutate in place
/// scan(0u64, |acc, v| acc + v)          // Fn(&B, &A) -> B     — return the new value
/// ```
///
/// Pick by the shape of the accumulator, not by taste:
///
/// * [`Fold`] when the accumulator is expensive to rebuild — a `Vec`, a map,
///   an order book. Mutating in place avoids constructing a whole new one per
///   tick.
/// * `scan` when the accumulator is a small `Copy` value, or when the update
///   is naturally an expression. This is also the shape
///   [`Iterator::fold`](std::iter::Iterator::fold) and Rx's `scan` use, so it
///   is what most callers reach for first.
///
/// `scan` clones the accumulator once per tick to emit it, exactly as
/// [`Fold`] does; the *extra* cost over `Fold` is the closure's returned
/// value being moved into the state slot, which is free for small `Copy`
/// types and is precisely what makes it the wrong choice for a large one.
///
/// The closure is `Fn`, not `FnMut` (see [`Map`]).
pub struct Scan<A, B, F>(PhantomData<(A, B, F)>);

#[op(build = scan, fluent, init_arg)]
impl<A, B, F> Op for Scan<A, B, F>
where
    A: 'static,
    B: Clone + 'static,
    F: Fn(&B, &A) -> B + 'static,
{
    type Cfg = F;
    type State = B;
    type In<'a> = (&'a A,);
    type Out = B;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut F, state: &mut B, input: (&A,), _ctx: &mut Ctx<'_>) -> Result<Tick<B>> {
        *state = cfg(state, input.0);
        Ok(Tick::Value(state.clone()))
    }
}

/// Running count of upstream ticks: 1, 2, 3, … Previously desugared to
/// [`Fold`] separately in the fluent layer *and* the `nitro!` macro (two
/// places that could drift); now one op both layers call.
pub struct Count<T>(PhantomData<T>);

#[op(build = count)]
impl<T: 'static> Op for Count<T> {
    type Cfg = ();
    type State = u64;
    type In<'a> = (&'a T,);
    type Out = u64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut u64,
        _input: (&T,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<u64>> {
        *state += 1;
        Ok(Tick::Value(*state))
    }
}

/// Collect every emitted value into a `Vec`, emitting the accumulated `Vec`
/// on each tick (cloned per tick — identical to its previous fold-based
/// desugar). Single-sourced for both layers, like [`Count`].
///
/// **This is a test/inspection instrument, not an output edge.** State grows
/// by one entry per tick for the whole run and every tick clones the whole
/// `Vec`, so it is `O(n)` memory and `O(n²)` copying over a run of `n` ticks —
/// unbounded in realtime, and enough to park a [`pool`](crate::pool) loan
/// forever (see the pool module docs). Use it where a run is bounded and
/// something afterwards needs the entire sequence in one value: asserting
/// exact values *and* tick times (`with_time().accumulate()`), or tying two
/// runs out against each other.
///
/// To get values *out* of a graph, reach for the streaming edges instead —
/// they emit as the run progresses, cost nothing per tick, and still print
/// what was seen if a later cycle aborts the run:
///
/// | want | use |
/// |---|---|
/// | print each value | [`Print`] (`.print()`) |
/// | log each value at a level | [`Logged`] (`.logged(..)`) |
/// | write/send each value, fallibly | [`Sink`] (`.for_each(..)`, `.for_each_mut(..)`) |
/// | observe in passing, infallibly | [`Inspect`] (`.inspect(..)`) |
/// | a bounded recent window | [`Window`] / [`Buffer`] |
pub struct Accumulate<T>(PhantomData<T>);

#[op(build = accumulate, fluent)]
impl<T: Clone + 'static> Op for Accumulate<T> {
    type Cfg = ();
    type State = Vec<T>;
    type In<'a> = (&'a T,);
    type Out = Vec<T>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut Vec<T>,
        input: (&T,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Vec<T>>> {
        state.push(input.0.clone());
        Ok(Tick::Value(state.clone()))
    }
}

/// Emits its (passive) source value whenever its trigger ticks. The trigger
/// is a unit stream: its *tick* is the activation condition (`passive = [0]`
/// marks the data edge as non-activating), and its unit value is ignored.
pub struct Sample<T>(PhantomData<T>);

#[op(build = sample, fluent, passive = [0])]
impl<T: Clone + 'static> Op for Sample<T> {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a T, &'a ());
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&T, &()),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        Ok(Tick::Value(input.0.clone()))
    }
}

/// A busy-poll source: the closure is called once on **every** engine cycle
/// (`Activation::ALWAYS`), ticking when it returns `Some`. This is the busy-spin
/// ingestion pattern — polling a ring buffer, socket, or channel via
/// `try_recv` — and it is *lossless and ordered*: one value per cycle, no
/// coalescing. A realtime run containing a poll source never parks. Realtime
/// only. (Threaded/async sources with the burst envelope are the
/// `external`/`channel` builder methods, which drain to a `Burst` — no
/// latest-wins.)
///
/// The closure is `Fn`: poll external resources through `&self` receivers
/// (e.g. `mpsc::Receiver::try_recv`) or interior mutability, not by
/// mutating captures (see [`Map`] for why).
pub struct Poll<T, F>(PhantomData<(T, F)>);

// `no_builder`: the interpreted [`Builder::poll`](crate::interp::Builder::poll)
// is hand-written because a poll source also sets the builder-level
// `has_always` / `re_runnable` flags, which a generated method cannot reach.
// The attribute is still needed for the `nitro!` forwarders — without them a
// busy-poll source has no compiled dispatch and cannot appear in `nitro!`.
#[op(build = poll, no_builder)]
impl<T, F> Op for Poll<T, F>
where
    T: Clone + 'static,
    F: Fn() -> Option<T> + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = ();
    type Out = T;
    const ACTIVATION: Activation = Activation::ALWAYS;

    fn cycle(cfg: &mut F, _state: &mut (), _input: (), _ctx: &mut Ctx<'_>) -> Result<Tick<T>> {
        Ok(match cfg() {
            Some(v) => Tick::Value(v),
            None => Tick::Quiet,
        })
    }
}

/// A sink: runs a side-effecting (fallible) closure on each source tick — the
/// graph's outbound edge (print, send, record). Emits `()` per tick so
/// downstream nodes can still observe its cadence. The closure returns
/// `Result` so an IO write failure aborts the run with context.
pub struct Sink<A, F>(PhantomData<(A, F)>);

#[op(build = for_each, fluent)]
impl<A, F> Op for Sink<A, F>
where
    A: 'static,
    F: Fn(&A) -> Result<()> + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a A,);
    type Out = ();
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut F, _state: &mut (), input: (&A,), _ctx: &mut Ctx<'_>) -> Result<Tick<()>> {
        cfg(input.0)?;
        Ok(Tick::Value(()))
    }
}

/// Runs a closure once at [`teardown`](Op::teardown), after the run ends —
/// even if a cycle aborted it. The legacy `finally` node: cleanup that must
/// happen regardless of how the run terminated. Passively observes its
/// source (so it never itself triggers a cycle); the closure sees the
/// source's last value. Emits nothing during the run.
pub struct Finally<A, F>(PhantomData<(A, F)>);

#[op(build = finally, fluent)]
impl<A, F> Op for Finally<A, F>
where
    A: Clone + Default + 'static,
    F: Fn(&A) -> Result<()> + 'static,
{
    type Cfg = F;
    /// Holds the source's last-seen value, replayed to the closure at
    /// teardown.
    type State = A;
    type In<'a> = (&'a A,);
    type Out = ();
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(_cfg: &mut F, state: &mut A, input: (&A,), _ctx: &mut Ctx<'_>) -> Result<Tick<()>> {
        *state = input.0.clone();
        Ok(Tick::Quiet)
    }

    fn teardown(cfg: &mut F, state: &mut A, _ctx: &mut Ctx<'_>) -> Result<()> {
        cfg(state)
    }
}

/// Joins two streams with a closure — the legacy `bimap` with two active
/// upstreams: ticks when either input ticks, reading both current values.
pub struct Join<A, B, C, F>(PhantomData<(A, B, C, F)>);

#[op(build = join, fluent)]
impl<A, B, C, F> Op for Join<A, B, C, F>
where
    A: 'static,
    B: 'static,
    C: Clone + 'static,
    F: Fn(&A, &B) -> C + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a A, &'a B);
    type Out = C;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut F, _state: &mut (), input: (&A, &B), _ctx: &mut Ctx<'_>) -> Result<Tick<C>> {
        Ok(Tick::Value(cfg(input.0, input.1)))
    }
}

/// Joins two streams with a *fallible* closure — the legacy `try_bimap`. The
/// `try_` counterpart to [`Join`]: any returned `Err` propagates to abort the
/// run with context. Each upstream is independently active or passive (an
/// engine dispatch concern); both values are always read. The closure is `Fn`,
/// like [`Join`].
pub struct TryJoin<A, B, C, F>(PhantomData<(A, B, C, F)>);

#[op(build = try_join, fluent)]
impl<A, B, C, F> Op for TryJoin<A, B, C, F>
where
    A: 'static,
    B: 'static,
    C: Clone + 'static,
    F: Fn(&A, &B) -> Result<C> + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a A, &'a B);
    type Out = C;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut F, _state: &mut (), input: (&A, &B), _ctx: &mut Ctx<'_>) -> Result<Tick<C>> {
        Ok(Tick::Value(cfg(input.0, input.1)?))
    }
}

/// The `nitro!`/compiled forwarder witness for [`join_passive`] — [`Join`]'s
/// semantics with the **second** edge passive (read but not activating: the
/// `bimap(Active, Passive)` shape a feedback input takes). This separate
/// witness exists because a second `#[op]` cannot be attached to [`Join`] and
/// the passive mask is the only thing that differs; carrying it lets
/// `#[op(passive = [1])]` emit both the `__wf_op_join_passive_*` forwarders
/// *and* the interpreted `Builder::join_passive`
/// (`bimap(_, true, _, false)`'s generated twin). `cycle` delegates to [`Join`]
/// so the semantics are single-sourced.
pub struct JoinPassive<A, B, C, F>(PhantomData<(A, B, C, F)>);

#[op(build = join_passive, fluent, passive = [1])]
impl<A, B, C, F> Op for JoinPassive<A, B, C, F>
where
    A: 'static,
    B: 'static,
    C: Clone + 'static,
    F: Fn(&A, &B) -> C + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a A, &'a B);
    type Out = C;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut F, state: &mut (), input: (&A, &B), ctx: &mut Ctx<'_>) -> Result<Tick<C>> {
        <Join<A, B, C, F> as Op>::cycle(cfg, state, input, ctx)
    }
}

/// The `nitro!`/compiled forwarder witness for [`try_join_passive`] — the
/// `try_` counterpart to [`JoinPassive`], delegating to [`TryJoin`] with the
/// second edge passive. See [`JoinPassive`] for why a separate witness is
/// needed.
pub struct TryJoinPassive<A, B, C, F>(PhantomData<(A, B, C, F)>);

#[op(build = try_join_passive, fluent, passive = [1])]
impl<A, B, C, F> Op for TryJoinPassive<A, B, C, F>
where
    A: 'static,
    B: 'static,
    C: Clone + 'static,
    F: Fn(&A, &B) -> Result<C> + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a A, &'a B);
    type Out = C;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(cfg: &mut F, state: &mut (), input: (&A, &B), ctx: &mut Ctx<'_>) -> Result<Tick<C>> {
        <TryJoin<A, B, C, F> as Op>::cycle(cfg, state, input, ctx)
    }
}

/// Delays its source by a fixed interval. The op that forced the retrofit's
/// `cycle_typed` tier — here it needs nothing special: `SCHEDULES` grants it
/// `Ctx::schedule`, its queue is ordinary `State`, and the upstream tick
/// flag arrives as input data instead of a `GraphState` lookup. Any engine
/// can run it.
pub struct Delay<T>(PhantomData<T>);

/// Pending values for a [`Delay`] op, plus whether the first upstream value
/// has been stored into the slot (via [`Tick::Silent`]).
pub struct DelayState<T: PartialEq> {
    queue: TimeQueue<T>,
    seeded: bool,
}

impl<T: PartialEq> Default for DelayState<T> {
    fn default() -> Self {
        Self {
            queue: TimeQueue::new(),
            seeded: false,
        }
    }
}

#[op(build = delay, fluent)]
impl<T: Clone + PartialEq + 'static> Op for Delay<T> {
    type Cfg = Duration;
    type State = DelayState<T>;
    /// Source value plus whether the source ticked this cycle.
    type In<'a> = (&'a T, bool);
    type Out = T;
    const ACTIVATION: Activation = Activation::SCHEDULES;

    fn cycle(
        cfg: &mut Duration,
        state: &mut DelayState<T>,
        input: (&T, bool),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        let (value, src_ticked) = input;
        let delay = NanoTime::from(*cfg);
        // Legacy parity: a zero delay emits inline in the *same* cycle
        // (scheduling `time + 0` would instead pop next cycle).
        if delay == NanoTime::ZERO {
            return Ok(if src_ticked {
                Tick::Value(value.clone())
            } else {
                Tick::Quiet
            });
        }
        if src_ticked {
            let at = ctx.time() + delay;
            state.queue.push(value.clone(), at);
            ctx.schedule(at);
        }
        let mut out = Tick::Quiet;
        while let Some(due) = state.queue.pop_if_pending(ctx.time()) {
            out = Tick::Value(due);
        }
        // Legacy parity: the first upstream value is stored into the slot
        // *without* ticking, so passive readers see it (not `T::default()`)
        // before the delay elapses.
        if matches!(out, Tick::Quiet) && src_ticked && !state.seeded {
            state.seeded = true;
            return Ok(Tick::Silent(value.clone()));
        }
        Ok(out)
    }
}

/// Merges two streams: the earliest-supplied input that ticked this cycle
/// wins. Tick flags arrive as input data, not via engine lookups.
pub struct Merge2<T>(PhantomData<T>);

#[op(build = merge, fluent)]
impl<T: Clone + 'static> Op for Merge2<T> {
    type Cfg = ();
    type State = ();
    type In<'a> = ((&'a T, bool), (&'a T, bool));
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: ((&T, bool), (&T, bool)),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        let ((a, a_ticked), (b, b_ticked)) = input;
        Ok(if a_ticked {
            Tick::Value(a.clone())
        } else if b_ticked {
            Tick::Value(b.clone())
        } else {
            Tick::Quiet
        })
    }
}

/// Merges **N** streams: the earliest-supplied input that ticked this cycle
/// wins — the n-ary generalisation of [`Merge2`], and the twin of legacy
/// wingfoil's `merge(vec)`.
///
/// It exists as its own op rather than as sugar over a chain of [`Merge2`]s
/// because the chain is not free: `merge_all`/`fan` used to left-fold into
/// `n - 1` binary merges, costing `n - 1` extra *nodes* and `n - 1` extra
/// *depth* against legacy's single node. On a busy 256-wide fan-in that
/// measured 1.86x legacy — a Phase 6 gate violation, not a tuning
/// opportunity. Rebalancing the chain recovers only the depth half; only a
/// real n-ary node removes both.
///
/// One of the catalog's two **variadic** ops (with [`CombineN`]): its `In` is a
/// *slice* of `(value, tick)` pairs rather than a fixed-arity tuple, so
/// `#[op]` cannot parse its shape. Its interpreted wiring
/// ([`Builder::merge_n`](crate::interp::Builder::merge_n)) and its
/// `nitro!`/compiled forwarders (below) are hand-written instead.
pub struct MergeN<T>(PhantomData<T>);

impl<T: Clone> MergeN<T> {
    /// The n-ary merge rule itself: the position of the earliest-supplied
    /// input that ticked, or `None` when none did.
    ///
    /// Factored out because the interpreted engine cannot call
    /// [`Op::cycle`] directly here. Building the `&[(&T, bool)]` slice
    /// `cycle` wants means borrowing every upstream slot at once, which for a
    /// 256-wide merge is a heap allocation on every cycle — exactly the cost
    /// this op exists to remove. The interpreted path therefore finds the
    /// winner from the engine's tick flags and borrows only *that* slot,
    /// routing through this shared rule so the two paths cannot disagree
    /// about which input wins.
    #[inline]
    pub fn winner(ticks: impl IntoIterator<Item = bool>) -> Option<usize> {
        ticks.into_iter().position(|ticked| ticked)
    }
}

impl<T: Clone + 'static> Op for MergeN<T> {
    type Cfg = ();
    type State = ();
    type In<'a> = &'a [(&'a T, bool)];
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: &[(&T, bool)],
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        Ok(
            match Self::winner(input.iter().map(|&(_, ticked)| ticked)) {
                Some(i) => Tick::Value(input[i].0.clone()),
                None => Tick::Quiet,
            },
        )
    }
}

// ---- `merge_n` nitro!/compiled forwarders (hand-written) -------------------
//
// `#[op]` generates these from an op's `In` shape; it cannot for a variadic
// op, so they are written out here. The `nitro!` emission calls them by the
// same naming convention as every generated family — the only difference is
// that a variadic node's `cycle_input` is a pair *slice* (`&[(v, t), ..]`)
// rather than a fixed-arity tuple, which is what lets one signature serve a
// fan-in of any width.

/// [`MergeN`] never schedules itself — it is driven purely by upstream ticks.
#[doc(hidden)]
pub const __WF_OP_MERGE_N_ACTIVATION: Activation = <MergeN<()> as Op>::ACTIVATION;

/// Bit i set = edge i is passive. A variadic merge is all-active by
/// construction: every input it is given can trigger it.
#[doc(hidden)]
pub const __WF_OP_MERGE_N_PASSIVE: u32 = 0;

#[doc(hidden)]
#[inline(always)]
pub fn __wf_op_merge_n_cycle<T: Clone + 'static>(
    __cfg: &mut (),
    __state: &mut (),
    __input: &[(&T, bool)],
    __ctx: &mut Ctx<'_>,
) -> Result<Tick<T>> {
    <MergeN<T> as Op>::cycle(__cfg, __state, __input, __ctx)
}

/// [`MergeN`] does not override `start`, so — exactly as `#[op]` does for a
/// hook-free op — this is a **fully erased** no-op. A forwarder carrying the
/// op's `T` would leave it un-inferable at the call site: `start` takes no
/// input, so nothing there anchors it.
#[doc(hidden)]
#[inline(always)]
pub fn __wf_op_merge_n_start<__Cfg, __State>(
    _cfg: &mut __Cfg,
    _state: &mut __State,
    _ctx: &mut Ctx<'_>,
) -> Result<()> {
    Ok(())
}

#[doc(hidden)]
#[inline(always)]
pub fn __wf_op_merge_n_stop<T: Clone + 'static>(
    __cfg: &mut (),
    __state: &mut (),
    __input: &[(&T, bool)],
    __ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = __input;
    <MergeN<T> as Op>::stop(__cfg, __state, __ctx)
}

#[doc(hidden)]
#[inline(always)]
pub fn __wf_op_merge_n_teardown<T: Clone + 'static>(
    __cfg: &mut (),
    __state: &mut (),
    __input: &[(&T, bool)],
    __ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = __input;
    <MergeN<T> as Op>::teardown(__cfg, __state, __ctx)
}

#[doc(hidden)]
#[inline(always)]
#[allow(clippy::unused_unit)]
pub fn __wf_op_merge_n_seed_state<__P>(_cfg: &__P) -> () {}

#[doc(hidden)]
#[inline(always)]
pub fn __wf_op_merge_n_seed_value<T: Default, __P>(_cfg: &__P) -> T {
    T::default()
}

/// Combine several same-type streams into one [`Burst`]: every input that
/// ticked *this* instant contributes its current value, in supplied order.
/// The legacy `combine` node, and the second of the catalog's two variadic ops
/// (with [`MergeN`]).
///
/// The distinction from [`MergeN`] is what each does with a tie. A merge picks
/// **one** winner and drops the rest; combine keeps **all** of them, which is
/// the burst pattern — same-instant values ride one burst, never latest-wins,
/// never dropped.
///
/// Like `MergeN` it is variadic, so `#[op]` cannot parse its `In` and both the
/// interpreted wiring ([`Builder::combine`](crate::interp::Builder::combine))
/// and the forwarders below are hand-written.
pub struct CombineN<T>(PhantomData<T>);

impl<T: Clone + Default> CombineN<T> {
    /// The rule for what a gathered burst *means*: a burst of every ticked
    /// input's value is a value; an empty one is no tick at all.
    ///
    /// Factored out for the same reason [`MergeN::winner`] is. The interpreted
    /// engine cannot call [`Op::cycle`] here — building the `&[(&T, bool)]`
    /// slice it takes means holding a borrow guard for *every* upstream slot at
    /// once, which is a per-cycle allocation on a wide fan-in. It therefore
    /// walks the engine's tick flags and borrows only the slots that ticked,
    /// then routes the result through this shared rule, so the two paths cannot
    /// disagree about the one thing that is easy to get wrong: a cycle where
    /// nothing ticked must stay quiet rather than emit an empty burst.
    ///
    /// (Only reachable via a shared scheduled wake — with no ticked upstream
    /// there is nothing to gather.)
    #[inline]
    pub fn emit(burst: Burst<T>) -> Tick<Burst<T>> {
        if burst.is_empty() {
            Tick::Quiet
        } else {
            Tick::Value(burst)
        }
    }
}

impl<T: Clone + Default + 'static> Op for CombineN<T> {
    type Cfg = ();
    type State = ();
    type In<'a> = &'a [(&'a T, bool)];
    type Out = Burst<T>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: &[(&T, bool)],
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Burst<T>>> {
        let mut burst = Burst::new();
        for &(value, ticked) in input {
            if ticked {
                burst.push(value.clone());
            }
        }
        Ok(Self::emit(burst))
    }
}

// ---- `combine` nitro!/compiled forwarders (hand-written) -------------------
//
// The `merge_n` block above explains the convention; these follow it exactly.
// The one shape difference is the output: `Burst<T>` rather than `T`, so the
// slot seed is an empty burst.

/// [`CombineN`] never schedules itself — it is driven purely by upstream ticks.
#[doc(hidden)]
pub const __WF_OP_COMBINE_ACTIVATION: Activation = <CombineN<()> as Op>::ACTIVATION;

/// Bit i set = edge i is passive. A variadic combine is all-active by
/// construction: every input it is given can trigger it.
#[doc(hidden)]
pub const __WF_OP_COMBINE_PASSIVE: u32 = 0;

#[doc(hidden)]
#[inline(always)]
pub fn __wf_op_combine_cycle<T: Clone + Default + 'static>(
    __cfg: &mut (),
    __state: &mut (),
    __input: &[(&T, bool)],
    __ctx: &mut Ctx<'_>,
) -> Result<Tick<Burst<T>>> {
    <CombineN<T> as Op>::cycle(__cfg, __state, __input, __ctx)
}

/// Fully erased, as [`__wf_op_merge_n_start`] is and for the same reason:
/// `start` takes no input, so a forwarder carrying `T` would leave it
/// un-inferable at the call site.
#[doc(hidden)]
#[inline(always)]
pub fn __wf_op_combine_start<__Cfg, __State>(
    _cfg: &mut __Cfg,
    _state: &mut __State,
    _ctx: &mut Ctx<'_>,
) -> Result<()> {
    Ok(())
}

#[doc(hidden)]
#[inline(always)]
pub fn __wf_op_combine_stop<T: Clone + Default + 'static>(
    __cfg: &mut (),
    __state: &mut (),
    __input: &[(&T, bool)],
    __ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = __input;
    <CombineN<T> as Op>::stop(__cfg, __state, __ctx)
}

#[doc(hidden)]
#[inline(always)]
pub fn __wf_op_combine_teardown<T: Clone + Default + 'static>(
    __cfg: &mut (),
    __state: &mut (),
    __input: &[(&T, bool)],
    __ctx: &mut Ctx<'_>,
) -> Result<()> {
    let _ = __input;
    <CombineN<T> as Op>::teardown(__cfg, __state, __ctx)
}

#[doc(hidden)]
#[inline(always)]
#[allow(clippy::unused_unit)]
pub fn __wf_op_combine_seed_state<__P>(_cfg: &__P) -> () {}

/// The output slot's seed: an **empty** burst, which is also what a quiet
/// cycle leaves in place — so a downstream passive read before the first tick
/// sees "nothing arrived", not a stale or defaulted value.
#[doc(hidden)]
#[inline(always)]
pub fn __wf_op_combine_seed_value<T: Default, __P>(_cfg: &__P) -> Burst<T> {
    Burst::new()
}

/// A source that never ticks — the legacy `never` node. It has no upstreams
/// and never schedules itself, so it stays [`Tick::Quiet`] for the whole run;
/// its unit value is never observed downstream. The idiomatic inert trigger —
/// e.g. a [`DelayWithReset`] that never resets degrades to a plain [`Delay`].
///
/// Kept a plain `Op` with a hand-written `Builder::never` (no `#[op]`):
/// nothing about the shape prevents the attribute — `#[op]` builds sources
/// fine — but a node that never ticks has no `nitro!`/compiled meaning worth
/// forwarding, so it stays an interpreted-engine primitive.
pub struct Never;

impl Op for Never {
    type Cfg = ();
    type State = ();
    type In<'a> = ();
    type Out = ();
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(_cfg: &mut (), _state: &mut (), _input: (), _ctx: &mut Ctx<'_>) -> Result<Tick<()>> {
        Ok(Tick::Quiet)
    }
}

/// Pending state for a [`DelayWithReset`] op: the queue of not-yet-due values
/// and whether the first upstream value has been stored into the slot (via
/// [`Tick::Silent`], as [`Delay`] does).
pub struct DelayWithResetState<T: PartialEq> {
    queue: TimeQueue<T>,
    initialized: bool,
}

impl<T: PartialEq> Default for DelayWithResetState<T> {
    fn default() -> Self {
        Self {
            queue: TimeQueue::new(),
            initialized: false,
        }
    }
}

/// [`Delay`] with a reset trigger — the legacy `delay_with_reset` node. When
/// the trigger ticks, the output snaps to the *current* upstream value and the
/// pending queue is cleared; otherwise it behaves exactly like [`Delay`]. The
/// two active inputs (upstream value and the trigger's tick) arrive as input
/// data — `(&value, upstream_ticked, trigger_ticked)` — mirroring the way
/// [`Delay`] receives its upstream tick flag; `SCHEDULES` grants the delayed
/// re-emit its `Ctx::schedule`.
///
/// No `#[op]` on *this* impl: its `In` puts two tick flags in a row with no
/// second value edge, which the uniform one-`(value, tick)`-pair-per-edge form
/// cannot express. [`DelayWithResetFwd`] below restates the same shape in the
/// two-edge form and carries the attribute for both engines.
pub struct DelayWithReset<T>(PhantomData<T>);

impl<T: Clone + PartialEq + 'static> Op for DelayWithReset<T> {
    type Cfg = Duration;
    type State = DelayWithResetState<T>;
    /// Upstream value, whether the upstream ticked, and whether the reset
    /// trigger ticked, this cycle.
    type In<'a> = (&'a T, bool, bool);
    type Out = T;
    const ACTIVATION: Activation = Activation::SCHEDULES;

    fn cycle(
        cfg: &mut Duration,
        state: &mut DelayWithResetState<T>,
        input: (&T, bool, bool),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        let (value, upstream_ticked, trigger_ticked) = input;
        // Reset wins over a same-cycle upstream tick (legacy checks the
        // trigger first): snap to the live upstream value, drop the queue.
        if trigger_ticked {
            state.queue.clear();
            state.initialized = true;
            return Ok(Tick::Value(value.clone()));
        }
        let delay = NanoTime::from(*cfg);
        // Legacy parity: a zero delay emits inline in the *same* cycle.
        if delay == NanoTime::ZERO {
            return Ok(if upstream_ticked {
                Tick::Value(value.clone())
            } else {
                Tick::Quiet
            });
        }
        let mut seed_first = false;
        if upstream_ticked {
            if !state.initialized {
                state.initialized = true;
                seed_first = true;
            }
            let at = ctx.time() + delay;
            state.queue.push(value.clone(), at);
            ctx.schedule(at);
        }
        let mut out = Tick::Quiet;
        while let Some(due) = state.queue.pop_if_pending(ctx.time()) {
            out = Tick::Value(due);
        }
        // Legacy parity: the first upstream value is stored into the slot
        // *without* ticking, so passive readers see it before the delay
        // elapses (identical to `Delay`'s first-value seeding).
        if matches!(out, Tick::Quiet) && seed_first {
            return Ok(Tick::Silent(value.clone()));
        }
        Ok(out)
    }
}

/// The `nitro!`/compiled forwarder witness for [`delay_with_reset`] — a thin
/// adapter over [`DelayWithReset`], delegating so its scheduling/seeding
/// semantics are single-sourced. The real op's `In = (&T, bool, bool)` (source
/// value, source tick, reset tick — the trigger's *value* is never read) is a
/// shape `#[op]` cannot parse from tokens (two consecutive tick flags with no
/// second value edge). This witness restates `In` in the uniform two-edge form
/// `(&T, bool, &U, bool)` that `#[op]` *does* parse — one `(value, tick)` pair
/// per edge — then drops the trigger's value when delegating. Both engines run
/// identical [`DelayWithReset::cycle`] logic.
///
/// `U` is the trigger stream's value type; it is generic because the reset
/// trigger may carry any payload (only its tick matters), matching the fluent
/// [`delay_with_reset`](crate::fluent::StreamOps::delay_with_reset)`<U>`.
pub struct DelayWithResetFwd<T, U>(PhantomData<(T, U)>);

#[op(build = delay_with_reset)]
impl<T: Clone + PartialEq + 'static, U: 'static> Op for DelayWithResetFwd<T, U> {
    type Cfg = Duration;
    type State = DelayWithResetState<T>;
    /// Source value, source tick, trigger value (ignored), trigger tick — the
    /// two-edge `(value, tick)`-per-edge form `#[op]` parses.
    type In<'a> = (&'a T, bool, &'a U, bool);
    type Out = T;
    const ACTIVATION: Activation = Activation::SCHEDULES;

    fn cycle(
        cfg: &mut Duration,
        state: &mut DelayWithResetState<T>,
        input: (&T, bool, &U, bool),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        let (value, upstream_ticked, _trigger_value, trigger_ticked) = input;
        <DelayWithReset<T> as Op>::cycle(cfg, state, (value, upstream_ticked, trigger_ticked), ctx)
    }
}

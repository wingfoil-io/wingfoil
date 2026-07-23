//! The core op vocabulary. Each op is a zero-sized *witness type* carrying
//! semantics in associated functions — never instantiated. Compare each
//! `cycle` body with the corresponding `MutableNode` impl in the main crate:
//! the logic is identical, but here it is written once and executed by every
//! engine, interpreted or compiled.
//!
//! Every op carries `#[op(build = name)]` (or hand-written equivalents),
//! which generates the `graph!` forwarder functions (`__wf_op_<name>_*`) that
//! all compiled/nested emission dispatches through, plus — for single-input
//! shapes — the interpreted [`Builder`](crate::interp::Builder) wiring method
//! (over `register_op1`), with the node label derived from `type_name`.
//! Attribute flags cover every shape in the catalog: `passive = [..]` marks
//! non-activating edges (sample), `init_arg` is the seeded-accumulator shape
//! (fold), and multi-input and source ops use `no_builder` to keep their
//! hand-written `Builder` methods.
//! See `docs/port-plan.md` "Adding an op" for the full recipe.

use std::collections::VecDeque;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::ops::Sub;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::op::{Activation, Ctx, Op, Tick};
use wingfoil::{NanoTime, TimeQueue};
use wingfoil_next_macros::op;

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

#[op(build = ticker, no_builder)]
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

#[op(build = constant, no_builder)]
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

#[op(build = map)]
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

#[op(build = try_map)]
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

#[op(build = map_filter)]
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

#[op(build = distinct)]
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

/// Emits the successive difference `value - previous`. Quiet on the first
/// value (no previous to subtract).
pub struct Difference<T>(PhantomData<T>);

#[op(build = difference)]
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

/// Passes through the first `limit` values, then stays quiet. `Cfg` is the
/// limit; `State` the count emitted so far.
pub struct Limit<T>(PhantomData<T>);

#[op(build = limit)]
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
/// `interval` has passed since the last emit. `Cfg` = interval, `State` =
/// last emit time.
pub struct Throttle<T>(PhantomData<T>);

impl<T: Clone + 'static> Op for Throttle<T> {
    type Cfg = NanoTime;
    type State = Option<NanoTime>;
    type In<'a> = (&'a T,);
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut NanoTime,
        state: &mut Option<NanoTime>,
        input: (&T,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        let now = ctx.time();
        let emit = match *state {
            None => true,
            Some(last) => now - last >= *cfg,
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

#[op(build = inspect)]
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

/// Passes each value through unchanged while buffering it, then prints the
/// whole buffer (`{value:?}` per line) at teardown. The classic `print` node,
/// whose buffered `Drop` becomes the op's [`teardown`](Op::teardown) hook —
/// so the summary still prints even if a cycle aborted the run. `State` is the
/// buffer.
pub struct Print<T>(PhantomData<T>);

impl<T: Clone + Debug + 'static> Op for Print<T> {
    type Cfg = ();
    type State = Vec<T>;
    type In<'a> = (&'a T,);
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        state: &mut Vec<T>,
        input: (&T,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        state.push(input.0.clone());
        Ok(Tick::Value(input.0.clone()))
    }

    fn teardown(_cfg: &mut (), state: &mut Vec<T>, _ctx: &mut Ctx<'_>) -> Result<()> {
        for val in state.iter() {
            println!("{val:?}");
        }
        Ok(())
    }
}

/// Pending state for a [`Timed`] op: the tick count and the wall-clock start
/// instant (set at [`start`](Op::start)).
#[derive(Default)]
pub struct TimedState {
    cycles: u64,
    wall_start: Option<Instant>,
}

/// Passes its source value through unchanged, recording the wall-clock start
/// at [`start`](Op::start) and printing a performance summary at
/// [`stop`](Op::stop) (tick count, wall-clock duration, per-tick average, and
/// the engine-time span covered). The classic `timed` node.
///
/// Deviations from classic (observable value stream is identical — a
/// pass-through — only the diagnostic differs): the summary goes to `stderr`
/// via `eprintln!` rather than `log::info!` (wingfoil-next has no `log`
/// dependency), and — since [`Ctx`] exposes no run mode — it always reports
/// the wall-vs-engine speedup rather than branching on historical/realtime.
pub struct Timed<T>(PhantomData<T>);

impl<T: Clone + 'static> Op for Timed<T> {
    type Cfg = ();
    type State = TimedState;
    type In<'a> = (&'a T,);
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn start(_cfg: &mut (), state: &mut TimedState, _ctx: &mut Ctx<'_>) -> Result<()> {
        state.wall_start = Some(Instant::now());
        Ok(())
    }

    fn cycle(
        _cfg: &mut (),
        state: &mut TimedState,
        input: (&T,),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        state.cycles += 1;
        Ok(Tick::Value(input.0.clone()))
    }

    fn stop(_cfg: &mut (), state: &mut TimedState, ctx: &mut Ctx<'_>) -> Result<()> {
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
/// (`interval`), plus a final flush on the last cycle. `Cfg` = interval;
/// `State` holds the next boundary and the pending buffer.
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

impl<T: Clone + 'static> Op for Window<T> {
    type Cfg = NanoTime;
    type State = WindowState<T>;
    type In<'a> = (&'a T,);
    type Out = Vec<T>;
    const ACTIVATION: Activation = Activation::NONE;

    fn start(cfg: &mut NanoTime, state: &mut WindowState<T>, ctx: &mut Ctx<'_>) -> Result<()> {
        state.next_window = ctx.time() + *cfg;
        Ok(())
    }

    fn cycle(
        cfg: &mut NanoTime,
        state: &mut WindowState<T>,
        input: (&T,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Vec<T>>> {
        let now = ctx.time();
        let mut out = None;
        if now >= state.next_window {
            if !state.buffer.is_empty() {
                out = Some(std::mem::take(&mut state.buffer));
            }
            // Advance the boundary past `now`, regardless of data.
            while state.next_window <= now {
                state.next_window = state.next_window + *cfg;
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

#[op(build = buffer)]
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

/// Combines three streams with a closure — the classic `trimap`. Ticks when
/// any active input ticks (active/passive is an engine dispatch concern);
/// all three values are read. `Fn`, like [`Join`].
pub struct Join3<A, B, C, D, F>(PhantomData<(A, B, C, D, F)>);

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

/// Combines three streams with a *fallible* closure — the classic
/// `try_trimap`. The `try_` counterpart to [`Join3`]: any returned `Err`
/// propagates to abort the run with context. The closure is `Fn`, like
/// [`Join3`].
pub struct TryJoin3<A, B, C, D, F>(PhantomData<(A, B, C, D, F)>);

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
pub struct WithTime<T>(PhantomData<T>);

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

#[op(build = ticked_at)]
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

#[op(build = ticked_at_elapsed)]
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

#[op(build = ewma)]
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
/// call-site sugar duplicated in the fluent layer and the `graph!` macro.
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

#[op(build = ewma_half_life)]
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

#[op(build = rolling_sum)]
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

#[op(build = rolling_mean)]
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
/// classic `RollingMomentStream` under `Weighting::Count`.
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

    /// Sample variance (ddof = 1) — the classic `Weighting::Count` convention.
    /// Yields `0.0` while fewer than two samples are present (rather than NaN).
    fn variance(&self) -> f64 {
        if self.count < 2 {
            return 0.0;
        }
        self.m2 / (self.count as f64 - 1.0)
    }
}

/// Rolling **sample** variance (ddof = 1) over the most recent `window` `f64`
/// samples. Matches the classic statistics adapter's `Weighting::Count`
/// convention: the divisor is `n - 1`, and the result is `0.0` until at least
/// two samples are in the window. `Cfg` = window.
pub struct RollingVar;

#[op(build = rolling_var)]
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

#[op(build = rolling_std)]
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
/// extreme — O(1) amortised per tick. Mirrors the classic `RollingExtremeStream`
/// (a monotonic deque, not a per-tick window scan, so the tick semantics match
/// the classic node exactly even though a scan would give the same values).
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

#[op(build = rolling_min)]
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

#[op(build = rolling_max)]
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
/// classic recompute-per-tick `WindowStream` (the median has no cheap
/// incremental form here).
#[derive(Default)]
pub struct RollingMedianState {
    buffer: VecDeque<f64>,
}

impl RollingMedianState {
    /// Push a sample (evicting the oldest past `window`) and return the median
    /// of the retained samples. An even-sized window averages the two middle
    /// values, so unit weights reproduce the ordinary median — the classic
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
/// the classic statistics adapter's count-weighted median. `Cfg` = window.
pub struct RollingMedian;

#[op(build = rolling_median)]
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

// ── time-windowed rolling statistics (Window::Time, count-weighted) ──────────
//
// Each op keeps the samples currently inside a bounded time window — the last
// `window` nanoseconds of graph time — and computes a statistic over them.
// Eviction semantics, reproduced from the classic `WindowStream` /
// `RollingMomentStream` `Window::Time` path (`wingfoil/src/adapters/statistics.rs`):
//
//   * on each tick the current sample is pushed at `now = ctx.time()`, then
//     every front entry whose age `now - t` is **strictly greater** than
//     `window` is evicted — so an entry exactly `window` old is retained (an
//     inclusive trailing boundary);
//   * the just-pushed sample has age 0, so it is always in window and the window
//     is **never empty** (classic has no empty-window instant — even a zero-width
//     window keeps the current sample); and
//   * `var`/`std` use the sample (ddof = 1) convention — `0.0` until at least two
//     samples are in the window.
//
// All are count-weighted (the ordinary statistic over the samples in the
// window). The time-*weighted* windowed combo (`Weighting::Time` over
// `Window::Time`) is a separate follow-up.

/// True when an entry at time `t` observed from `now` has aged past `window`
/// nanoseconds (the classic strictly-greater trailing boundary). Time is
/// monotonic, so `now >= t` and the subtraction never underflows.
fn aged_out(now: NanoTime, t: NanoTime, window: u64) -> bool {
    u64::from(now) - u64::from(t) > window
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

/// Sum over a bounded time window of the last `window` (a [`NanoTime`] duration)
/// of graph time — O(1) per tick. Mirrors the classic `sum(Window::Time(_))`.
pub struct TimeWindowedSum;

#[op(build = time_windowed_sum)]
impl Op for TimeWindowedSum {
    type Cfg = NanoTime;
    type State = TimeWindowSumState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut NanoTime,
        state: &mut TimeWindowSumState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let out = state.push(ctx.time(), *input.0, u64::from(*cfg));
        Ok(Tick::Value(out))
    }
}

/// Ring-buffer state for the time-windowed mean / variance / std: the
/// `(time, value)` samples in the window plus incrementally maintained
/// count-weighted moments (Welford's algorithm with exact removal), so a tick
/// is O(1) amortised. Mirrors the classic `RollingMomentStream` under
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
/// Mirrors the classic `mean(Window::Time(_), Weighting::Count)`.
pub struct TimeWindowedMean;

#[op(build = time_windowed_mean)]
impl Op for TimeWindowedMean {
    type Cfg = NanoTime;
    type State = TimeWindowMomentState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut NanoTime,
        state: &mut TimeWindowMomentState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.push(ctx.time(), *input.0, u64::from(*cfg));
        // The current sample is always in window, so `count >= 1` and `mean`
        // holds the window mean (equal to the sample on the first tick).
        Ok(Tick::Value(state.mean))
    }
}

/// **Sample** variance (ddof = 1) over a bounded time window — O(1) amortised
/// per tick. Mirrors the classic `variance(Window::Time(_), Weighting::Count)`.
pub struct TimeWindowedVar;

#[op(build = time_windowed_var)]
impl Op for TimeWindowedVar {
    type Cfg = NanoTime;
    type State = TimeWindowMomentState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut NanoTime,
        state: &mut TimeWindowMomentState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.push(ctx.time(), *input.0, u64::from(*cfg));
        Ok(Tick::Value(state.variance()))
    }
}

/// **Sample** standard deviation over a bounded time window — the square root of
/// [`TimeWindowedVar`], clamped at zero before the root so a constant window
/// yields `0.0`, not `NaN`. Mirrors the classic `std(Window::Time(_),
/// Weighting::Count)`.
pub struct TimeWindowedStd;

#[op(build = time_windowed_std)]
impl Op for TimeWindowedStd {
    type Cfg = NanoTime;
    type State = TimeWindowMomentState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut NanoTime,
        state: &mut TimeWindowMomentState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        state.push(ctx.time(), *input.0, u64::from(*cfg));
        Ok(Tick::Value(state.variance().max(0.0).sqrt()))
    }
}

/// Monotonic-deque state for the time-windowed min & max. Holds `(time, value)`
/// candidates kept monotonic in value and increasing in time front-to-back, so
/// the front is always the window extreme — O(1) amortised per tick. Unlike the
/// classic time-windowed `WindowStream` (a per-tick scan), this uses the same
/// monotonic-deque trick as the classic *count*-windowed `RollingExtremeStream`,
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
/// per tick. Mirrors the classic `min(Window::Time(_))`.
pub struct TimeWindowedMin;

#[op(build = time_windowed_min)]
impl Op for TimeWindowedMin {
    type Cfg = NanoTime;
    type State = TimeWindowExtremeState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut NanoTime,
        state: &mut TimeWindowExtremeState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let out = state.push(ctx.time(), *input.0, u64::from(*cfg), true);
        Ok(Tick::Value(out))
    }
}

/// Maximum over a bounded time window, via a monotonic deque — O(1) amortised
/// per tick. Mirrors the classic `max(Window::Time(_))`.
pub struct TimeWindowedMax;

#[op(build = time_windowed_max)]
impl Op for TimeWindowedMax {
    type Cfg = NanoTime;
    type State = TimeWindowExtremeState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut NanoTime,
        state: &mut TimeWindowExtremeState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let out = state.push(ctx.time(), *input.0, u64::from(*cfg), false);
        Ok(Tick::Value(out))
    }
}

/// Ring-buffer state for the time-windowed median — retains the `(time, value)`
/// samples in the window and recomputes the median (sort) each tick, matching
/// the classic recompute-per-tick `WindowStream` (the median has no cheap
/// incremental form here).
#[derive(Default)]
pub struct TimeWindowMedianState {
    buffer: VecDeque<(NanoTime, f64)>,
}

impl TimeWindowMedianState {
    /// Push `sample` at `now`, evict aged entries, and return the median of the
    /// retained samples. An even count averages the two middle values, so this
    /// reproduces the classic count-weighted median.
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
/// matching the classic count-weighted median. Mirrors the classic
/// `median(Window::Time(_), Weighting::Count)`.
pub struct TimeWindowedMedian;

#[op(build = time_windowed_median)]
impl Op for TimeWindowedMedian {
    type Cfg = NanoTime;
    type State = TimeWindowMedianState;
    type In<'a> = (&'a f64,);
    type Out = f64;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        cfg: &mut NanoTime,
        state: &mut TimeWindowMedianState,
        input: (&f64,),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<f64>> {
        let out = state.push(ctx.time(), *input.0, u64::from(*cfg));
        Ok(Tick::Value(out))
    }
}

/// Emits its source value when the condition stream's current value is true.
pub struct Filter<T>(PhantomData<T>);

#[op(build = filter, no_builder)]
impl<T: Clone + 'static> Op for Filter<T> {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a T, &'a bool);
    type Out = T;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(
        _cfg: &mut (),
        _state: &mut (),
        input: (&T, &bool),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        if *input.1 {
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
pub struct Fold<A, B, F>(PhantomData<(A, B, F)>);

#[op(build = fold, no_builder, init_arg)]
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

/// Running count of upstream ticks: 1, 2, 3, … Previously desugared to
/// [`Fold`] separately in the fluent layer *and* the `graph!` macro (two
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
pub struct Accumulate<T>(PhantomData<T>);

#[op(build = accumulate)]
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

#[op(build = sample, no_builder, passive = [0])]
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

#[op(build = for_each)]
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
/// even if a cycle aborted it. The classic `finally` node: cleanup that must
/// happen regardless of how the run terminated. Passively observes its
/// source (so it never itself triggers a cycle); the closure sees the
/// source's last value. Emits nothing during the run.
pub struct Finally<A, F>(PhantomData<(A, F)>);

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

/// Joins two streams with a closure — the classic `bimap` with two active
/// upstreams: ticks when either input ticks, reading both current values.
pub struct Join<A, B, C, F>(PhantomData<(A, B, C, F)>);

#[op(build = join, no_builder)]
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

/// Joins two streams with a *fallible* closure — the classic `try_bimap`. The
/// `try_` counterpart to [`Join`]: any returned `Err` propagates to abort the
/// run with context. Each upstream is independently active or passive (an
/// engine dispatch concern); both values are always read. The closure is `Fn`,
/// like [`Join`].
pub struct TryJoin<A, B, C, F>(PhantomData<(A, B, C, F)>);

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

#[op(build = delay, no_builder)]
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
        // Classic parity: a zero delay emits inline in the *same* cycle
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
        // Classic parity: the first upstream value is stored into the slot
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

#[op(build = merge, no_builder)]
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

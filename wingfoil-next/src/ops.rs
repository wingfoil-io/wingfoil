//! The core op vocabulary. Each op is a zero-sized *witness type* carrying
//! semantics in associated functions — never instantiated. Compare each
//! `cycle` body with the corresponding `MutableNode` impl in the main crate:
//! the logic is identical, but here it is written once and executed by every
//! engine, interpreted or compiled.

use std::marker::PhantomData;
use std::ops::Sub;

use anyhow::Result;

use crate::op::{Caps, Ctx, Op, Tick};
use wingfoil::{NanoTime, TimeQueue};

/// Ticks at a fixed interval, anchored to its first activation to avoid
/// drift. `Cfg` = interval, `State` = the last scheduled time.
pub struct Ticker;

impl Op for Ticker {
    type Cfg = NanoTime;
    type State = Option<NanoTime>;
    type In<'a> = ();
    type Out = ();
    const CAPS: Caps = Caps::SCHEDULES;

    fn cycle(
        cfg: &mut NanoTime,
        state: &mut Option<NanoTime>,
        _input: (),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<()>> {
        let next = match *state {
            Some(t) => t + *cfg,
            None => ctx.time() + *cfg,
        };
        *state = Some(next);
        ctx.schedule(next);
        Ok(Tick::Value(()))
    }

    fn start(_cfg: &mut NanoTime, _state: &mut Option<NanoTime>, ctx: &mut Ctx<'_>) -> Result<()> {
        ctx.schedule(ctx.start_time());
        Ok(())
    }
}

/// Ticks once with a fixed value, on the first cycle.
pub struct Const<T>(PhantomData<T>);

impl<T: Clone + 'static> Op for Const<T> {
    type Cfg = T;
    type State = ();
    type In<'a> = ();
    type Out = T;
    const CAPS: Caps = Caps::SCHEDULES;

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
    const CAPS: Caps = Caps::NONE;

    fn cycle(cfg: &mut F, _state: &mut (), input: (&A,), _ctx: &mut Ctx<'_>) -> Result<Tick<B>> {
        Ok(Tick::Value(cfg(input.0)))
    }
}

/// Applies a *fallible* closure to its input, propagating any error to abort
/// the run with context. The `try_` counterpart to [`Map`]; the closure is
/// `Fn` for the same drift-safety reason (see [`Map`]).
pub struct TryMap<A, B, F>(PhantomData<(A, B, F)>);

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
    const CAPS: Caps = Caps::NONE;

    fn cycle(cfg: &mut F, _state: &mut (), input: (&A,), _ctx: &mut Ctx<'_>) -> Result<Tick<B>> {
        Ok(Tick::Value(cfg(input.0)?))
    }
}

/// Emits a value with a per-value tick decision: the closure returns
/// `(value, emit?)`, so it maps and filters in one pass. The `filter_map` of
/// the catalog. `Fn`, like [`Map`].
pub struct MapFilter<A, B, F>(PhantomData<(A, B, F)>);

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
    const CAPS: Caps = Caps::NONE;

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

impl<T: Clone + PartialEq + 'static> Op for Distinct<T> {
    type Cfg = ();
    type State = Option<T>;
    type In<'a> = (&'a T,);
    type Out = T;
    const CAPS: Caps = Caps::NONE;

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

impl<T> Op for Difference<T>
where
    T: Clone + Sub<Output = T> + 'static,
{
    type Cfg = ();
    type State = Option<T>;
    type In<'a> = (&'a T,);
    type Out = T;
    const CAPS: Caps = Caps::NONE;

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

impl<T: Clone + 'static> Op for Limit<T> {
    type Cfg = u32;
    type State = u32;
    type In<'a> = (&'a T,);
    type Out = T;
    const CAPS: Caps = Caps::NONE;

    fn cycle(cfg: &mut u32, state: &mut u32, input: (&T,), _ctx: &mut Ctx<'_>) -> Result<Tick<T>> {
        if *state >= *cfg {
            Ok(Tick::Quiet)
        } else {
            *state += 1;
            Ok(Tick::Value(input.0.clone()))
        }
    }
}

/// Emits its source value when the condition stream's current value is true.
pub struct Filter<T>(PhantomData<T>);

impl<T: Clone + 'static> Op for Filter<T> {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a T, &'a bool);
    type Out = T;
    const CAPS: Caps = Caps::NONE;

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
    const CAPS: Caps = Caps::NONE;

    fn cycle(cfg: &mut F, state: &mut B, input: (&A,), _ctx: &mut Ctx<'_>) -> Result<Tick<B>> {
        cfg(state, input.0);
        Ok(Tick::Value(state.clone()))
    }
}

/// Emits its (passive) source value whenever its trigger ticks. The trigger
/// carries no value — its tick is the activation condition, supplied by the
/// engine's dispatch, so it does not appear in `In`.
pub struct Sample<T>(PhantomData<T>);

impl<T: Clone + 'static> Op for Sample<T> {
    type Cfg = ();
    type State = ();
    type In<'a> = (&'a T,);
    type Out = T;
    const CAPS: Caps = Caps::NONE;

    fn cycle(_cfg: &mut (), _state: &mut (), input: (&T,), _ctx: &mut Ctx<'_>) -> Result<Tick<T>> {
        Ok(Tick::Value(input.0.clone()))
    }
}

/// An external (threaded/async) source: values arrive on a channel from a
/// producer thread or async task, which wakes the kernel after sending. If
/// several values arrive between cycles the *latest wins* (a burst-collapse;
/// accumulate upstream of the channel if every value matters). Realtime
/// runs only.
pub struct External<T>(PhantomData<T>);

impl<T: Clone + 'static> Op for External<T> {
    type Cfg = std::sync::mpsc::Receiver<T>;
    type State = ();
    type In<'a> = ();
    type Out = T;
    const CAPS: Caps = Caps::THREADED;

    fn cycle(
        cfg: &mut std::sync::mpsc::Receiver<T>,
        _state: &mut (),
        _input: (),
        _ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        let mut latest = None;
        while let Ok(v) = cfg.try_recv() {
            latest = Some(v);
        }
        Ok(match latest {
            Some(v) => Tick::Value(v),
            None => Tick::Quiet,
        })
    }
}

/// A busy-poll source: the closure is called once on **every** engine cycle
/// (`Caps::ALWAYS`), ticking when it returns `Some`. This is the busy-spin
/// ingestion pattern — polling a ring buffer, socket, or channel via
/// `try_recv` — and it is *lossless and ordered*: one value per cycle, no
/// coalescing (contrast [`External`], which collapses to latest-wins).
/// A realtime run containing a poll source never parks. Realtime only.
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
    const CAPS: Caps = Caps::ALWAYS;

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

impl<A, F> Op for Sink<A, F>
where
    A: 'static,
    F: Fn(&A) -> Result<()> + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a A,);
    type Out = ();
    const CAPS: Caps = Caps::NONE;

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
    const CAPS: Caps = Caps::NONE;

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
    const CAPS: Caps = Caps::NONE;

    fn cycle(cfg: &mut F, _state: &mut (), input: (&A, &B), _ctx: &mut Ctx<'_>) -> Result<Tick<C>> {
        Ok(Tick::Value(cfg(input.0, input.1)))
    }
}

/// Delays its source by a fixed interval. The op that forced the retrofit's
/// `cycle_typed` tier — here it needs nothing special: `SCHEDULES` grants it
/// `Ctx::schedule`, its queue is ordinary `State`, and the upstream tick
/// flag arrives as input data instead of a `GraphState` lookup. Any engine
/// can run it.
pub struct Delay<T>(PhantomData<T>);

/// Pending values for a [`Delay`] op.
pub struct DelayState<T: PartialEq> {
    queue: TimeQueue<T>,
}

impl<T: PartialEq> Default for DelayState<T> {
    fn default() -> Self {
        Self {
            queue: TimeQueue::new(),
        }
    }
}

impl<T: Clone + PartialEq + 'static> Op for Delay<T> {
    type Cfg = NanoTime;
    type State = DelayState<T>;
    /// Source value plus whether the source ticked this cycle.
    type In<'a> = (&'a T, bool);
    type Out = T;
    const CAPS: Caps = Caps::SCHEDULES;

    fn cycle(
        cfg: &mut NanoTime,
        state: &mut DelayState<T>,
        input: (&T, bool),
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<T>> {
        let (value, src_ticked) = input;
        if src_ticked {
            let at = ctx.time() + *cfg;
            state.queue.push(value.clone(), at);
            ctx.schedule(at);
        }
        let mut out = Tick::Quiet;
        while let Some(due) = state.queue.pop_if_pending(ctx.time()) {
            out = Tick::Value(due);
        }
        Ok(out)
    }
}

/// Merges two streams: the earliest-supplied input that ticked this cycle
/// wins. Tick flags arrive as input data, not via engine lookups.
pub struct Merge2<T>(PhantomData<T>);

impl<T: Clone + 'static> Op for Merge2<T> {
    type Cfg = ();
    type State = ();
    type In<'a> = ((&'a T, bool), (&'a T, bool));
    type Out = T;
    const CAPS: Caps = Caps::NONE;

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

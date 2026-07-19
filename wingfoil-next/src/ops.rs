//! The core op vocabulary. Each op is a zero-sized *witness type* carrying
//! semantics in associated functions — never instantiated. Compare each
//! `cycle` body with the corresponding `MutableNode` impl in the main crate:
//! the logic is identical, but here it is written once and executed by every
//! engine, interpreted or compiled.

use std::marker::PhantomData;

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
    ) -> Tick<()> {
        let next = match *state {
            Some(t) => t + *cfg,
            None => ctx.time() + *cfg,
        };
        *state = Some(next);
        ctx.schedule(next);
        Tick::Value(())
    }

    fn start(_cfg: &mut NanoTime, _state: &mut Option<NanoTime>, ctx: &mut Ctx<'_>) {
        ctx.schedule(ctx.start_time());
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

    fn cycle(cfg: &mut T, _state: &mut (), _input: (), _ctx: &mut Ctx<'_>) -> Tick<T> {
        Tick::Value(cfg.clone())
    }

    fn start(_cfg: &mut T, _state: &mut (), ctx: &mut Ctx<'_>) {
        ctx.schedule(ctx.start_time());
    }
}

/// Applies a closure to its input. The closure *is* the config — a type
/// parameter, never boxed, so compiled engines monomorphize straight through
/// it.
pub struct Map<A, B, F>(PhantomData<(A, B, F)>);

impl<A, B, F> Op for Map<A, B, F>
where
    A: 'static,
    B: Clone + 'static,
    F: FnMut(&A) -> B + 'static,
{
    type Cfg = F;
    type State = ();
    type In<'a> = (&'a A,);
    type Out = B;
    const CAPS: Caps = Caps::NONE;

    fn cycle(cfg: &mut F, _state: &mut (), input: (&A,), _ctx: &mut Ctx<'_>) -> Tick<B> {
        Tick::Value(cfg(input.0))
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

    fn cycle(_cfg: &mut (), _state: &mut (), input: (&T, &bool), _ctx: &mut Ctx<'_>) -> Tick<T> {
        if *input.1 {
            Tick::Value(input.0.clone())
        } else {
            Tick::Quiet
        }
    }
}

/// Folds inputs into an accumulator; emits the accumulator after each fold.
/// The accumulator is the state — initialised by the engine, so `fold` can
/// start from any value, not just `Default`.
pub struct Fold<A, B, F>(PhantomData<(A, B, F)>);

impl<A, B, F> Op for Fold<A, B, F>
where
    A: 'static,
    B: Clone + 'static,
    F: FnMut(&mut B, &A) + 'static,
{
    type Cfg = F;
    type State = B;
    type In<'a> = (&'a A,);
    type Out = B;
    const CAPS: Caps = Caps::NONE;

    fn cycle(cfg: &mut F, state: &mut B, input: (&A,), _ctx: &mut Ctx<'_>) -> Tick<B> {
        cfg(state, input.0);
        Tick::Value(state.clone())
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

    fn cycle(_cfg: &mut (), _state: &mut (), input: (&T,), _ctx: &mut Ctx<'_>) -> Tick<T> {
        Tick::Value(input.0.clone())
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
    ) -> Tick<T> {
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
        out
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
    ) -> Tick<T> {
        let ((a, a_ticked), (b, b_ticked)) = input;
        if a_ticked {
            Tick::Value(a.clone())
        } else if b_ticked {
            Tick::Value(b.clone())
        } else {
            Tick::Quiet
        }
    }
}

//! The [`Op`] trait: node semantics as pure, monomorphizable functions.
//!
//! An op is *only* semantics. It owns no storage (state is an associated
//! type the engine instantiates wherever it likes), holds no upstream
//! pointers (inputs are passed in, typed, per cycle), and touches the engine
//! only through the narrow [`Ctx`] — with a `const` activation declaration
//! ([`Activation`]) that tells engines statically whether it ever will.

use anyhow::Result;
use wingfoil::codegen::Kernel;
use wingfoil::{NanoTime, TimeQueue};

/// Static declaration of how the engine must activate an op — beyond a plain
/// upstream data-tick.
///
/// Because the activation is `const`, engines can specialise on it at
/// compile time: an op with `schedules: false` can never be activated by a
/// callback, so a compiled schedule emits no dirty check for it and an
/// interpreted engine can skip its callback bookkeeping. This replaces the
/// retrofit's name-based `can_receive_callbacks` allowlist with a contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Activation {
    /// The op registers time callbacks (via [`Ctx::schedule`]) in `cycle` or
    /// `start`, and can therefore be activated without an upstream tick.
    pub schedules: bool,
    /// The op is fed by an external thread or async task that wakes the
    /// kernel (via a [`KernelWaker`](wingfoil::codegen::KernelWaker)).
    /// Realtime mode only — external events have no place in a deterministic
    /// historical replay, and engines reject the combination.
    pub threaded: bool,
    /// The op must be cycled on *every* engine cycle, with no activation —
    /// a busy-poll source (ring buffer, socket, channel drained by
    /// `try_recv`). A realtime run containing such an op becomes a
    /// busy-spin loop: the kernel never parks, cycles run back-to-back,
    /// and each cycle polls the op once. Realtime mode only — in a
    /// historical replay there is no external resource to poll, and
    /// engines reject the combination.
    pub always: bool,
}

impl Activation {
    pub const NONE: Activation = Activation {
        schedules: false,
        threaded: false,
        always: false,
    };
    pub const SCHEDULES: Activation = Activation {
        schedules: true,
        threaded: false,
        always: false,
    };
    pub const THREADED: Activation = Activation {
        schedules: false,
        threaded: true,
        always: false,
    };
    pub const ALWAYS: Activation = Activation {
        schedules: false,
        threaded: false,
        always: true,
    };

    /// True if this op can be activated by kernel callbacks (scheduled or
    /// external) — i.e. its dispatch condition needs a dirty check.
    /// (An `always` op needs no dirty check either — it runs
    /// unconditionally.)
    pub const fn callback_activated(&self) -> bool {
        self.schedules || self.threaded
    }
}

/// The outcome of one op cycle: either the op ticked and produced a value,
/// or it stayed quiet. Replaces the `bool` + hidden-value-slot side channel
/// of the old `MutableNode::cycle`.
///
/// # Contract gap: "update the value slot without ticking"
///
/// There is deliberately **no** `Silent(T)` variant that would update a node's
/// value slot while suppressing the downstream tick. Classic wingfoil's
/// `delay` uses exactly that ("store the first upstream value without
/// ticking", so passive readers see it before the delay elapses). Because an
/// op cannot express it, the two ops that need engine-owned init/timing
/// behaviour — `delay`'s first-value seeding and its zero-delay inline emit —
/// apply it at the **engine** level instead (see `Builder::delay` in
/// `interp.rs` and the Delay branch in the `graph!` macro's `node_dispatch`),
/// kept in lockstep across interpreted/compiled/nested. If a third op ever
/// needs the same shape, promote it to a real `Tick::Silent(T)` variant here
/// and teach all three engines to store-without-ticking on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick<T> {
    Value(T),
    Quiet,
}

/// The engine services an op may use, scoped to the current node.
///
/// Deliberately narrow — time and self-scheduling only. This is the entire
/// surface area an op has on the engine, which is what makes ops executable
/// by any engine — including a *composite* engine: a compiled sub-graph
/// mounted as one node inside an interpreted graph hands its inner ops a
/// [`nested`](Ctx::nested) context whose schedules land in the composite's
/// private queue instead of the outer kernel.
pub struct Ctx<'a> {
    time: NanoTime,
    start_time: NanoTime,
    is_last_cycle: bool,
    sink: Sink<'a>,
}

enum Sink<'a> {
    /// Schedules go straight to the run's kernel, keyed by graph node index.
    Kernel { kernel: &'a mut Kernel, node: usize },
    /// Schedules go to a composite's private queue, keyed by *inner* node
    /// index. The composite demultiplexes them on its next activation and
    /// forwards only the earliest time to the outer kernel.
    Queue {
        queue: &'a mut TimeQueue<usize>,
        node: usize,
    },
}

impl<'a> Ctx<'a> {
    pub fn new(kernel: &'a mut Kernel, node: usize) -> Self {
        Self {
            time: kernel.time(),
            start_time: kernel.start_time(),
            is_last_cycle: kernel.is_last_cycle(),
            sink: Sink::Kernel { kernel, node },
        }
    }

    /// A context for an op nested inside a composite node: time comes from
    /// the outer run, schedules go to the composite's private queue.
    /// `is_last_cycle` is not propagated into islands (the composite runs its
    /// own inner schedule), so a boundary-flush op inside an island flushes
    /// only on window boundaries, not at the outer run's end — a documented
    /// island limitation.
    #[doc(hidden)]
    pub fn nested(
        time: NanoTime,
        start_time: NanoTime,
        queue: &'a mut TimeQueue<usize>,
        node: usize,
    ) -> Self {
        Self {
            time,
            start_time,
            is_last_cycle: false,
            sink: Sink::Queue { queue, node },
        }
    }

    /// Current engine time.
    pub fn time(&self) -> NanoTime {
        self.time
    }

    /// The run's start time.
    pub fn start_time(&self) -> NanoTime {
        self.start_time
    }

    /// Whether this is the final cycle of the run. Buffering ops flush their
    /// pending contents when true.
    pub fn is_last_cycle(&self) -> bool {
        self.is_last_cycle
    }

    /// Schedule this node to be activated at `at`. Only meaningful for ops
    /// declaring [`Activation::SCHEDULES`].
    pub fn schedule(&mut self, at: NanoTime) {
        match &mut self.sink {
            Sink::Kernel { kernel, node } => kernel.schedule(*node, at),
            Sink::Queue { queue, node } => queue.push(*node, at),
        }
    }
}

/// Node semantics, single-sourced for every engine.
///
/// - `Cfg` — construction-time configuration, including closures (a map's
///   `F` *is* its config). Held by the engine, passed in by `&mut`.
/// - `State` — per-node mutable state. Owned by the engine: boxed in an
///   interpreted engine, a local in a compiled runner.
/// - `In<'a>` — the typed inputs for one cycle, passed in by the engine
///   (values by reference, tick flags where the op needs them). Ops never
///   reach upstream themselves.
/// - `Out` — the produced value type.
///
/// `cycle`, `start`, `stop` and `teardown` are associated functions, not
/// methods: an `Op` type is a *witness* for semantics, never instantiated.
/// Engines monomorphize these functions directly, which is what makes the
/// compiled path possible without duplicating any logic.
///
/// Every lifecycle function is fallible (`anyhow::Result`) — an op doing IO
/// (a socket read, a codec decode) must be able to abort the run with
/// context rather than panic. `cycle` returns `Result<Tick<Out>>`, keeping
/// the two axes distinct: `Tick::Quiet` is ordinary control flow (a quiet
/// cycle, the hot path), `Err` is failure (cold, aborts the run). Merging
/// them would cost `?` and the anyhow chain for no gain; for an infallible
/// op the `Ok(..)` is constant-folded away after monomorphization, leaving
/// no branch in the binary.
pub trait Op: 'static {
    type Cfg: 'static;
    type State: 'static;
    type In<'a>;
    type Out: Clone + 'static;
    const ACTIVATION: Activation;

    fn cycle(
        cfg: &mut Self::Cfg,
        state: &mut Self::State,
        input: Self::In<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Result<Tick<Self::Out>>;

    /// Called once before the first cycle. Sources use this to schedule
    /// their first activation; IO ops open resources here.
    #[allow(unused_variables)]
    fn start(cfg: &mut Self::Cfg, state: &mut Self::State, ctx: &mut Ctx<'_>) -> Result<()> {
        Ok(())
    }

    /// Called once after the last cycle, before [`teardown`](Op::teardown).
    /// Runs even if a cycle aborted the run, so ops can flush or unwind
    /// cleanly. A `stop` error is reported but does not mask an earlier
    /// cycle error.
    #[allow(unused_variables)]
    fn stop(cfg: &mut Self::Cfg, state: &mut Self::State, ctx: &mut Ctx<'_>) -> Result<()> {
        Ok(())
    }

    /// Called once at the very end of the run, after [`stop`](Op::stop).
    /// The place to release resources (close sockets, files). Also runs
    /// after a cycle error.
    #[allow(unused_variables)]
    fn teardown(cfg: &mut Self::Cfg, state: &mut Self::State, ctx: &mut Ctx<'_>) -> Result<()> {
        Ok(())
    }
}

/// [`Op::cycle`] with the config taken by value — for callers (the `graph!`
/// macro's compiled expansion) that rebuild a zero-capture closure config per
/// call. Taking the closure as a *direct* argument lets rustc defer its
/// signature inference until the sibling `input` argument has resolved the
/// op's value types; behind `&mut` that deferral does not apply.
#[doc(hidden)]
pub fn cycle_owned_cfg<O: Op>(
    mut cfg: O::Cfg,
    state: &mut O::State,
    input: O::In<'_>,
    ctx: &mut Ctx<'_>,
) -> Result<Tick<O::Out>> {
    O::cycle(&mut cfg, state, input, ctx)
}

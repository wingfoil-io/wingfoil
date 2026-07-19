//! The [`Op`] trait: node semantics as pure, monomorphizable functions.
//!
//! An op is *only* semantics. It owns no storage (state is an associated
//! type the engine instantiates wherever it likes), holds no upstream
//! pointers (inputs are passed in, typed, per cycle), and touches the engine
//! only through the narrow [`Ctx`] — with a `const` capability declaration
//! ([`Caps`]) that tells engines statically whether it ever will.

use wingfoil::NanoTime;
use wingfoil::codegen::Kernel;

/// Static capability declaration for an op type.
///
/// Because capabilities are `const`, engines can specialise on them at
/// compile time: an op with `schedules: false` can never be activated by a
/// callback, so a compiled schedule emits no dirty check for it and an
/// interpreted engine can skip its callback bookkeeping. This replaces the
/// retrofit's name-based `can_receive_callbacks` allowlist with a contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// The op registers time callbacks (via [`Ctx::schedule`]) in `cycle` or
    /// `start`, and can therefore be activated without an upstream tick.
    pub schedules: bool,
    /// The op is fed by an external thread or async task that wakes the
    /// kernel (via a [`KernelWaker`](wingfoil::codegen::KernelWaker)).
    /// Realtime mode only — external events have no place in a deterministic
    /// historical replay, and engines reject the combination.
    pub threaded: bool,
}

impl Caps {
    pub const NONE: Caps = Caps {
        schedules: false,
        threaded: false,
    };
    pub const SCHEDULES: Caps = Caps {
        schedules: true,
        threaded: false,
    };
    pub const THREADED: Caps = Caps {
        schedules: false,
        threaded: true,
    };

    /// True if this op can be activated by kernel callbacks (scheduled or
    /// external) — i.e. its dispatch condition needs a dirty check.
    pub const fn callback_activated(&self) -> bool {
        self.schedules || self.threaded
    }
}

/// The outcome of one op cycle: either the op ticked and produced a value,
/// or it stayed quiet. Replaces the `bool` + hidden-value-slot side channel
/// of the old `MutableNode::cycle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick<T> {
    Value(T),
    Quiet,
}

/// The engine services an op may use, scoped to the current node.
///
/// Deliberately narrow — time and self-scheduling only. This is the entire
/// surface area an op has on the engine, which is what makes ops executable
/// by any engine.
pub struct Ctx<'a> {
    kernel: &'a mut Kernel,
    node: usize,
}

impl<'a> Ctx<'a> {
    pub fn new(kernel: &'a mut Kernel, node: usize) -> Self {
        Self { kernel, node }
    }

    /// Current engine time.
    pub fn time(&self) -> NanoTime {
        self.kernel.time()
    }

    /// The run's start time.
    pub fn start_time(&self) -> NanoTime {
        self.kernel.start_time()
    }

    /// Schedule this node to be activated at `at`. Only meaningful for ops
    /// declaring [`Caps::SCHEDULES`].
    pub fn schedule(&mut self, at: NanoTime) {
        self.kernel.schedule(self.node, at);
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
/// `cycle` and `start` are associated functions, not methods: an `Op` type
/// is a *witness* for semantics, never instantiated. Engines monomorphize
/// these functions directly, which is what makes the compiled path possible
/// without duplicating any logic.
pub trait Op: 'static {
    type Cfg: 'static;
    type State: 'static;
    type In<'a>;
    type Out: Clone + 'static;
    const CAPS: Caps;

    fn cycle(
        cfg: &mut Self::Cfg,
        state: &mut Self::State,
        input: Self::In<'_>,
        ctx: &mut Ctx<'_>,
    ) -> Tick<Self::Out>;

    /// Called once before the first cycle. Sources use this to schedule
    /// their first activation.
    #[allow(unused_variables)]
    fn start(cfg: &mut Self::Cfg, state: &mut Self::State, ctx: &mut Ctx<'_>) {}
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
) -> Tick<O::Out> {
    O::cycle(&mut cfg, state, input, ctx)
}

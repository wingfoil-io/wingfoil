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
}

impl Caps {
    pub const NONE: Caps = Caps { schedules: false };
    pub const SCHEDULES: Caps = Caps { schedules: true };
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

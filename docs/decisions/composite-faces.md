# Composite faces in `nitro!` — decision

**Question.** A fluent face that wires *several* nodes — the execution
layer's `algo`, `oms`, `fix_bridge` and `reconcile`, each one op plus the
projections that split its output — could not be written inside `nitro!`. A
compiled graph had to spell the op at its build name through a test-local
wrapper and re-derive the projections by hand, which is the drift the
no-table design (`macro-extensibility-decision.md`) exists to prevent. Can a
compiled graph call such a face verbatim?

**Answer.** Yes, by handing `nitro!` the face's body and inlining it. A
**composite** is a definition written once in `nitro!`'s grammar (which is
also valid fluent Rust). `nitro!` binds a tuple to a composite call
(`let (d, p) = parents.algo(..);`) and inlines the body as ordinary nodes, so
the compiled graph is exactly the graph the fluent face wires, op for op. The
same definition, emitted as a plain function, *is* the fluent face's
implementation, so the two cannot drift.

A face that is one op under another name does not need any of this: it is an
`#[op(alias = ..)]`, which may also reorder edges (#1049).

## How the body reaches the macro

A proc macro sees tokens, never another item's body, so the body has to come
to it: the classic macro callback.

1. A `nitro!` body binds a tuple to `recv.face(..)`, and `nitro!` has no
   definition for `face` yet. It expands to `__wf_composite_face! { @nitro
   [known..] <its own input> }`.
2. The face's **shell**, a `macro_rules!` its author writes, forwards that
   to `composite!` along with the definition.
3. `composite!` re-invokes `nitro!` with the definition added under
   `@composites [..]`. One round trip per distinct composite. A composite
   whose body calls another resolves the inner one the same way.

`__wf_composite_face!(define)` is the shell's other use: it emits the
definition as a function, which the extension trait's method calls.

**The shell is hand-written because it has to be.** A `#[macro_export]`
macro *generated* by a proc macro cannot be re-exported by path
(rust-lang/rust#52234, deny-by-default and slated to become a hard error, and
not `allow`able below crate level). The re-export (`pub use
crate::__wf_composite_face;`) is what lets a glob import of the face's module,
the import that already brings its op's forwarders, bring the shell too.
Experiment confirmed both halves: the generated form is refused, and the
hand-written one re-exports and resolves out of crate.

## Inlining rules

Renaming is done on the syntax tree. Method names, fields and multi-segment
paths are never touched.

- A stream parameter becomes the stream passed. A builder parameter (first
  only, for a face on the builder) becomes the graph's builder.
- A plain argument is bound once to a fresh local where the call stands, as
  the fluent call evaluates it once. `tests/composite_face.rs` counts the
  evaluations. The exception is a literal closure passed where every use
  in the body is an op's *only* config argument: it is substituted there,
  so the op still sees a literal closure (the inference deferral `nitro!`
  depends on). Anywhere else, such as the last field of a tuple `Cfg`, it is
  bound like any other plain argument, so the tuple stays whole. Its
  parameters then want annotating, as for any named closure.
- A body binding may not reuse a parameter's name (a `let` or a closure
  parameter), because each name is renamed once for the whole body. A
  shell must hand back the face it is named for. Both are refused with a
  message rather than producing a wrong graph or recursing.
- Every binding becomes `wf_anon_c<k>_<name>`, except the ones the tail
  returns, which take the call site's pattern names (`_` keeps the fresh
  one).

## Builder faces

A builder face is a chain on a typestate builder:
`desired.wire_oms(config, epoch, &reports).trading_state(&t).build()`. It
reaches `nitro!` the same way, through a shell, but its definition is a
`builder` declaration rather than a function. The declaration lists:

- the opening parameters;
- each option method, as one parameter, optionally with a default (a
  closure on the builder, `|g| g.never()`);
- `build(..) =>` arms naming the face the chain stands for, chosen by which
  options without a default the chain names (`build(reading)` for
  `oms_reading`).

Inlining binds the arguments as a composite does, wires each missing
default, and leaves `let <pat> = <the arm's face call>;`. That call then
inlines as a composite or dispatches as an op (`wire_position` builds the
aliased `position` op).

- **The tell is the name as well as the shape.** The chain must end in an
  argument-less `.build()` and start with `wire_<face>`. `build` alone
  would make `.print().build()`, typed by habit, call a shell nobody wrote
  instead of getting `Stream::build`'s advice.
- **The fluent builder stays hand-written.** Its `build()` calls the arm's
  face function, so that half is one source. The defaults are the same
  chain written twice: the declaration's, and the builder's `optional()`.
  The short-versus-long parity tests (`surface_builder_faces`,
  `surface_oms_builder`) hold them together. Generating the typestate
  builder from the declaration would remove the duplication, but its doc
  surface is the public API and stays written by hand.
- A default's value comes from a fn item (`edge::filler`), not
  `|(): &()| Burst::new()`. A compiled graph re-emits a literal closure at
  each lifecycle call, and in those copies nothing pins a closure whose
  output only its consumer fixes. `never` became an `#[op]` so a default can
  spell it.

## Rejected

- **A composite as one compiled closure**, `fn face(..) -> impl FnMut(..)`
  owning its inner state. It would avoid the round trip, but it re-implements
  the island machinery: a private time queue and dirty bits for inner
  schedulers, and the stop/teardown phases. That is per composite, for a
  boundary that inlining makes free.
- **Detecting composites by name.** The op set is open, so the macro cannot
  tell a composite from an op. The tuple pattern is the tell: an op has one
  output. A single-output face is an alias, or a composite bound to a
  one-tuple.
- **A proc-macro attribute that writes the shell.** See #52234 above.

## Constraints a composite author inherits

- The body names no type parameter of the composite. Its call site has none,
  so closures stay unannotated or annotate concrete types. A value whose type
  comes from a generic closure's return (a reading `R`) is cloned with
  `Clone::clone(x)`, not `x.clone()`: the compiled tier resolves `R` only
  after the closure is checked.
- The body's ops take what `nitro!` takes. In particular, an op whose `Cfg`
  is a tuple ending in a closure takes a fn item there (`no_reading`), not a
  literal closure.
- Out of crate, the shell calls `::wingfoil::composite!`. In crate it calls
  `$crate::composite!`, and `$crate::` paths work in the body.

## What shipped

`composite!` and the `@composites` pass in `wingfoil-derive`
(`src/composite.rs`). The `algo`, `oms`, `oms_reading`, `fix_bridge` and
`reconcile` composites, each with an op-level fluent trait (`AlgoOpOps`,
`OmsOpOps`, `FixBridgeOpOps`, `ReconcileOpOps`) that replaced the test-local
wrappers. Pinned on every tier by `op_completeness.rs` (`surface_faces`,
`surface_reconcile_face`), `composite_face.rs` (user-position shells: a
plain argument, a closure argument, nested composites, a builder face, a
repeated call, `_`), and `trybuild/unknown_composite.rs`. Builder faces:
`wire_oms`, `wire_position` and `wire_reconcile`, pinned short form against
long by `surface_oms_builder` and `surface_builder_faces`.

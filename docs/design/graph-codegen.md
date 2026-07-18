# Design: executor-free code generation for wingfoil graphs

Status: **draft / research** · Scope: `wingfoil` core · Owner: TBD

## 1. Summary

Today a wingfoil graph is *interpreted*: the fluent API builds a DAG of
`Rc<RefCell<dyn Node>>` values, and `Graph::run()` walks it every engine cycle
via dynamic dispatch, `RefCell` borrows, and a dirty-set scheduler.

This document proposes a second, opt-in back-end that **compiles a graph into
plain, monomorphized Rust** — one `struct` of state plus one straight-line
`cycle()` function — with no vtables, no `RefCell`, no `Rc` clones, no
scheduler bookkeeping. The interpreter stays the default; codegen is a fast
path for fine-grained, high-frequency graphs (HFT / DSP / tight arithmetic
pipelines).

The headline design is an **attribute macro over the unchanged fluent API**:

```rust
#[wingfoil::compiled]      // ← delete this line and it runs on the interpreter
fn strategy() -> impl CompiledStream<u64> {
    let a = ticker(PERIOD).count().map(|x| x + 1);
    let b = a.map(|x| x * 2);
    let c = a.map(|x| x * 3);
    bimap(Dep::Active(b), Dep::Active(c), |x, y| x + y)
}
```

The same source compiles under both back-ends, which makes differential
testing (interpreted vs compiled, byte-identical output) trivial and airtight.

Two scope decisions:

- **IO keeps the existing channel pattern verbatim.** Adapters are unchanged;
  they remain opaque roots/leaves that `notify()` a channel and read/write a
  side buffer. Only the *interior* dispatch is replaced by generated code.
- **No dynamic graph.** `add_upstream` / `remove_node` and the `dynamic-graph`
  feature are out of scope. A fixed topology is a precondition for codegen.

## 2. Why codegen must happen before type erasure

A *built* graph cannot be codegen'd at runtime:

- operator transforms are stored as `Box<dyn Fn(IN) -> OUT>`
  (`nodes/map.rs:15`, `nodes/fold.rs`, `nodes/bimap.rs`) — the closure source
  is unrecoverable;
- concrete node types are erased behind `Rc<dyn Node>` / `Rc<dyn Stream<T>>`
  (`types.rs:211`, `types.rs:307`).

So any codegen must run **while the closures are still visible** — either as
token trees (a macro re-parsing the fluent syntax before erasure) or as
generic type parameters (letting rustc's monomorphizer do the flattening).
Those two mechanisms give the two viable designs in §5.

## 3. Where the interpreter spends its cycles

Per node, per cycle, the hot path (`graph.rs:924` `cycle_node`) pays:

| Cost | Source |
|---|---|
| `Rc::clone` of the node handle | `graph.rs:932` `node.clone().cycle(...)` |
| `dyn Node` vtable dispatch → `impl Node for RefCell` | `types.rs:215` |
| `RefCell::borrow_mut` flag check | `types.rs:216` |
| per upstream read: vtable + `borrow()` + **clone** | `map.rs:21` `peek_value()` → `types.rs:269` |
| `Box<dyn Fn>` indirect call | `map.rs:21` `(self.func)(...)` |
| dirty-set push + dedup per active downstream | `graph.rs:942`, `mark_dirty` `graph.rs:814` |
| per-cycle `reset()` over three vecs | `graph.rs:952` |
| timer / ready-channel scheduling | `TimeQueue`, `select!` `graph.rs:410` |

For a node doing real work this is noise. For `map(|x| x+1)` or `add(&a,&b)`
it is 10–50× the actual arithmetic. That regime — fine-grained,
high-tick-rate — is where codegen pays off (the `breadth_first` example /
`benches/bfs_vs_dfs`).

Everything in that table is **statically resolvable at wiring time**. The
layer each node lands in (`graph.rs:774`, `layer = max(upstream.layer + 1)`)
is the schedule generated code bakes in. Active-vs-passive edges and the
"`Ok(false)` suppresses downstream" convention become compile-time control
flow.

## 4. Target output: flat SSA sweep

Whatever the front-end, the emitted form is the same: one `struct` holding
only *persistent* state, and one `cycle()` in topological order where
"did an active upstream tick" is an SSA boolean. For the diamond graph

```
ticks ── map(+1) = a ─┬─ map(*2) = b ─┐
                      └─ map(*3) = c ─┴─ zip(+) = out
```

the target output is:

```rust
#[derive(Default)]
struct Pipeline {
    t: u64,                     // the only persistent state (source counter)
}

impl Pipeline {
    #[inline]
    fn cycle(&mut self) -> u64 {
        self.t += 1;
        let a = self.t + 1;     // map(+1) — computed ONCE
        let b = a * 2;          // fan-out = reuse the local `a`
        let c = a * 3;          //   no Rc, no RefCell, no memo, no Option
        b + c                   // zip
    }
}
```

Allocation rules:

- **stateful** nodes (`fold`, `scan`, `count`, `delay`, feedback source) →
  `struct` fields persisting across cycles;
- **passively-read or output** nodes → fields (latest value must survive
  between cycles);
- everything else → fused locals, no storage at all.

The fan-out property matters: this is a **push sweep**, so a shared node is
computed once into a single-owner local and simply reused — the dedup that
the interpreter achieves dynamically with `node_dirty` (`graph.rs:814`) is
achieved structurally, for free.

### 4.1 Guarded form (conditional ticks, active/passive)

With filters and mixed edges, tick propagation lowers to booleans:

```rust
fn cycle(&mut self, fired: &SourceFired) {
    // layer 0 — sources
    let a_ticked = fired.contains(SRC_A) && { self.a = self.src_a.drain_latest(); true };
    let b_ticked = fired.contains(SRC_B) && { self.b = self.src_b.drain_latest(); true };

    // sa = a.map(*2)             (active upstream a)
    let sa_ticked = a_ticked && { self.sa = self.a * 2.0; true };

    // mix = bimap(Active(sa), Passive(b))
    //   fires only when sa ticks; reads b's *current field* (passive edge)
    let _mix_ticked = sa_ticked && { self.mix = self.sa + self.b; true };

    let _ = b_ticked;   // passive edge: deliberately does not propagate
}
```

Three semantic rules become compile-time control flow: active edges feed the
downstream guard; passive edges are plain field reads that never trigger;
layer order guarantees a passively-read field is refreshed before it is read
in any cycle where it also ticked — matching `peek_value` semantics exactly.

## 5. Front-ends: how to keep the fluent API

"Generate code" does not require this project to print source. Rust has two
codegen mechanisms we can drive:

1. **rustc's monomorphizer** — if the fluent chain builds a fully *concrete*
   type instead of `Rc<dyn Stream>`, `#[inline]` does the flattening;
2. **an attribute macro** that re-parses the *existing* fluent syntax before
   type erasure and emits §4 directly.

### 5.1 Approach E (headline): `#[wingfoil::compiled]` attribute macro

The existing fluent syntax **is** the DSL. The macro sees the function body as
a token stream *before* anything becomes `Rc<dyn Stream>` or `Box<dyn Fn>`,
so the type-erasure objection in §2 does not apply. It:

1. pattern-matches the operator vocabulary (`ticker`, `constant`, `map`,
   `filter_value`, `bimap`, `fold`, `scan`, `count`, `sample`, `zip`,
   `source`, `sink`, …) over the `let`-bindings and the tail expression;
2. builds a symbolic DAG (bindings give node identity; reuse of a binding is
   fan-out);
3. topologically orders it — insertion order of bindings already is a valid
   order, since an expression can only reference existing bindings;
4. emits the §4 `struct` + `cycle()`, splicing the user's closures in
   verbatim (they monomorphize; captures still work because the tokens are
   emitted at the same scope).

Properties:

- **The fluent API is unchanged character-for-character** — one attribute is
  added. Removing the attribute runs the identical body on the interpreter.
- **Dual-backend differential testing**: the same function compiles both
  ways; a test harness runs both over identical historical input and asserts
  byte-identical output.
- Precedent: CubeCL's `#[cube]` re-parses ordinary-looking Rust into GPU
  kernels; `#[tokio::main]` and similar body-rewriting attributes.

Limitations (stated honestly):

- The macro sees *syntax*, not semantics. A helper
  `fn make_signal(...) -> Rc<dyn Stream<f64>>` called inside the body is
  opaque: it must either also be annotated (macro-inlined) or become an
  interpreter-boundary node. v0: clear compile error.
- Operators outside the known vocabulary → clear compile error (later:
  fall back to embedding the interpreter for that subgraph).
- Every new operator needs a lowering rule.

### 5.2 Approach D (macro-free stepping stone): typed graph builder

Fluent-ish API, zero macros; rustc's monomorphizer is the code generator.
Handles are **`Copy`, zero-sized type-level indices**; nodes live by value in
one owned tuple — no `Rc`, no `RefCell`, no memo:

```rust
let g = GraphBuilder::new();
let (g, t)   = g.ticker(PERIOD);          // t: Handle<0, u64>  (Copy, ZST)
let (g, a)   = g.map(t, |x| x + 1);       // a: Handle<1, u64>
let (g, b)   = g.map(a, |x| x * 2);
let (g, c)   = g.map(a, |x| x * 3);       // fan-out: reuse `a` — it's Copy
let (g, out) = g.bimap(b, c, |x, y| x + y);
let mut p = g.freeze();                   // one owned value, no dyn/Rc/RefCell
p.cycle();
let v: u64 = p.read(out);
```

Mechanics: builder state is a growing tuple type
(`GraphBuilder<(Ticker, (MapN<F1, u64>, ...))>`); each node is
`struct MapN<F, T> { f: F, value: T, ticked: bool }` with its upstream as a
type-level index; `cycle()` is a trait-recursive walk that, after
monomorphization + `#[inline]`, LLVM flattens to exactly the §4 body.

Two properties fall out for free:

- **Topological order and acyclicity by construction** — a handle can only
  reference an already-added node, so insertion order is the schedule; no
  layering pass, no cycle detection.
- **Compute-once fan-out without memoization** — push sweep: each node runs
  once per `cycle()`; downstreams read `value` as a plain field.

Costs: the builder must be threaded linearly (`let (g, x) = g...`; the type
changes each step, so `&mut` chaining is impossible), and the graph's *type*
is as deep as the node count — fine to ~tens of nodes, painful beyond.
Approach D is the right prototype order: it validates the push-sweep
semantics cheaply and is the semantic reference for what E emits flat.

### 5.3 Rejected/deferred alternatives

**B — nested generic combinators** (`a.map(f).filter(g)` returning
`Filter<Map<Src,F>,G>`, iterator-style). Fuses beautifully for *linear*
chains, but fan-out breaks it: pull-based sharing forces
`Rc<Shared<RefCell<…> + per-cycle memo>>` back in, and diamond graphs produce
pathologically deep types that cannot be stored in fields without re-boxing
(re-erasing). A compiled prototype comparing B vs the §4 flat form on the
diamond graph confirmed identical semantics and the machinery gap. Rejected
as the mainline; the linear-fusion insight survives inside D/E.

**F — runtime graph export via token capture.** For topologies shaped at
runtime (config-driven), compile-time front-ends cannot see the graph. An
escape hatch exists: `map!(|x| x * 2)` stashes `stringify!`'d tokens
alongside the compiled closure, and `graph.export_rust()` writes a `.rs` for
a second build phase. Works, but closures capturing locals cannot be exported
(captures must be lifted to parameters). Niche tool; deferred indefinitely.

**Runtime JIT (Cranelift)** over the existing boxed closures: removes
bookkeeping but keeps calling through `Box<dyn Fn>` — the read path survives.
High complexity, partial win. Rejected.

## 6. IO: keep the channel pattern verbatim

Every realtime/threaded source already talks to the engine through exactly
two things, neither of which is the executor:

1. **a wakeup channel** — `ReadyNotifier { node_index, sender }`
   (`graph.rs:163`); the adapter's thread calls `.notify()` when data lands;
2. **a side buffer** it owns (e.g. `Rc<RefCell<TimeQueue<T>>>`) that the
   thread writes and the node drains on wake.

Generated code preserves both unchanged. Adapters need **zero changes**. Each
IO source gets a stable compile-time id used on the channel; sinks are opaque
`publish()` calls at the tail of the sweep.

```rust
struct Pipeline {
    ema: f64,                       // fold accumulator + output
    src_md: SourceHandle<Quote>,    // owns buffer + thread (unchanged adapter)
    sink_signal: SinkHandle<f64>,
}
const SRC_MD: usize = 0;

impl Pipeline {
    #[inline]
    fn cycle(&mut self, fired: &SourceFired) {
        if fired.contains(SRC_MD) {
            let q = self.src_md.drain_latest();        // was peek_value()
            let mid = (q.bid + q.ask) / 2.0;           // map (fused local)
            self.ema = 0.9 * self.ema + 0.1 * mid;     // fold
            self.sink_signal.publish(self.ema);        // sink (adapter write)
        }
    }
}
```

The realtime driver **is** `wait_ready_callback` / `process_callbacks_realtime`
(`graph.rs:410`, `graph.rs:884`), specialized. It drains the whole channel
before running a single fused cycle, exactly as the interpreter does
(`graph.rs:873`):

```rust
loop {
    // block until a source is ready OR the next timer is due
    select! {
        recv(ready_rx) -> idx => { if let Ok(i) = idx { fired.set(i); } }
        default(next_timer - now) => {}
    }
    while let Ok(i)   = ready_rx.try_recv()                  { fired.set(i); }
    while let Some(i) = timers.pop_if_pending(NanoTime::now()) { fired.set(i); }

    p.cycle(&fired);          // one fused sweep over the union of fired sources
    fired.clear();
}
```

The channel is the only dynamic element, runs at the external-event rate (not
the compute rate), and is inherent to IO — keeping it costs nothing on the
hot path.

**Historical mode has no channel at all** — the interpreter already forbids
ready callbacks there (`graph.rs:843`). The historical driver merges the
sources' `TimeQueue`s into one timeline and calls `cycle()` per timestamp:
fully straight-line, zero synchronization. This is where the differential
test lives and where the biggest speedup shows up.

## 7. What is and isn't compilable

| Category | v0 | Notes |
|---|---|---|
| map, filter, bimap, trimap, zip, reduce | ✅ | fuse into locals |
| fold, scan, count, accumulate | ✅ | struct fields |
| constant, ticker, timed sources | ✅ | drive the driver via a minimal `TimeQueue` |
| delay, scheduled callbacks | ✅ | need the side `TimeQueue`, not the full engine |
| IO sources / sinks (kafka, zmq, web…) | ✅ (as boundary) | channel + buffer unchanged; not inlined |
| feedback loops | v1 | "write field this cycle, read next" + `+1ns` schedule (`feedback.rs:54`) |
| async/tokio adapters | v1 | runtime on the side, same channel seam |
| helper fns returning `Rc<dyn Stream>` | ❌ v0 | must be annotated or → compile error (§5.1) |
| dynamic graph | ❌ | out of scope by decision |

## 8. Correctness strategy

The load-bearing safety mechanism is the **dual-backend differential test**:
because §5.1 compiles the *same source* both ways, the harness runs the
identical function body interpreted (`RunMode::HistoricalFrom`) and compiled
over identical historical input and asserts byte-identical output. Wire the
compiled path into `benches/bfs_vs_dfs` as an extra contender to quantify the
speedup on the canonical fan-out graph.

Invariants each diff run checks:

1. tick / no-tick propagation (`filter` returning `Ok(false)` stops
   downstream);
2. active vs passive triggering;
3. fan-out dedup (a shared node computes once per cycle — `node_dirty`,
   `graph.rs:814`);
4. layer/topological ordering of reads;
5. feedback `+1ns` timing (when v1 lands).

## 9. Incremental plan

1. **Prototype approach D** (typed builder, no macro) for the v0 operator
   set — validates push-sweep semantics, fan-out-as-field-read, and the
   driver loop cheaply. Serves as the semantic reference.
2. **`wingfoil-codegen` proc-macro crate** implementing
   `#[wingfoil::compiled]` (approach E): parse the fluent body, build the
   symbolic DAG, emit §4.
3. **v0 operator set:** `constant`/`ticker`/`count` sources;
   `map`/`filter_value`/`bimap`/`fold`/`scan` ops; one channel-backed IO
   source + one sink.
4. **Differential test harness** (same body, both back-ends, historical).
5. **Benchmark** vs interpreter on `breadth_first` and map/filter chains.
6. **v1:** feedback, delay/timers polish, async adapters, annotated helper
   functions.

Also worth measuring early (cheap, de-risks the ceiling): partial
non-codegen wins — `Box<dyn Fn>` → generic `F` on operator structs (helps
the interpreter too), dropping `RefCell` on the single-threaded path,
`enum`-dispatch instead of `dyn`. If these recover most of the gap for real
workloads, full codegen may not be worth a second back-end to maintain.

## 10. Risks

- **Payoff is workload-specific** — near-zero for heavy-per-node or IO-bound
  graphs. Position as a targeted feature, not a default.
- **Second execution semantics** — tick propagation, passive reads, and
  (later) feedback timing must match the interpreter exactly. The
  differential test is non-negotiable, and the shared-source property of
  approach E is what makes it trustworthy.
- **Macro opacity limits** — helper functions and out-of-vocabulary
  operators fail loudly in v0; the fallback story (boundary nodes) is v1+.
- **Maintenance coupling** — every new operator needs a lowering rule in the
  macro or an explicit "unsupported" error.

# Design: executor-free code generation for wingfoil graphs

Status: **draft / research** · Scope: `wingfoil` core · Owner: TBD

## 1. Summary

Today a wingfoil graph is *interpreted*: the fluent API builds a DAG of
`Rc<RefCell<dyn Node>>` values, and `Graph::run()` walks it every engine cycle
via dynamic dispatch, `RefCell` borrows, and a dirty-set scheduler.

This document proposes an alternative back-end that **compiles a graph into
plain, monomorphized Rust** — one `struct` of state plus one straight-line
`cycle()` function — with no vtables, no `RefCell`, no `Rc` clones, no
scheduler bookkeeping. The interpreter stays the default; codegen is an opt-in
fast path for fine-grained, high-frequency graphs (HFT / DSP / tight
arithmetic pipelines).

The key realisation that shapes the whole design:

> A built graph cannot be codegen'd. Operator transforms are stored as
> `Box<dyn Fn(IN) -> OUT>` (`nodes/map.rs:15`, `nodes/fold.rs`, `nodes/bimap.rs`),
> and concrete node types are erased behind `Rc<dyn Node>` / `Rc<dyn Stream>`.
> Once the graph exists at runtime, the closures and types needed to emit source
> are gone.

Therefore codegen must happen **at compile time**, from a macro front-end that
still sees the closures as token trees. This proposal is a `pipeline! { … }`
proc-macro, not a `graph.to_rust()` method.

Two scope decisions for v0:

- **IO keeps the existing channel pattern verbatim.** IO adapters are unchanged;
  they remain opaque roots/leaves that `notify()` a channel and read/write a side
  buffer. Only the *interior* dispatch is replaced by generated code.
- **No dynamic graph.** `add_upstream` / `remove_node` and the `dynamic-graph`
  feature are out of scope. A fixed topology is a precondition for codegen anyway.

## 2. Where the interpreter spends its cycles

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

For a node doing real work this is noise. For `map(|x| x+1)` or
`add(&a,&b)` it is 10–50× the actual arithmetic. That regime — fine-grained,
high-tick-rate — is the only place codegen pays off, and it is exactly the niche
wingfoil targets (the `breadth_first` example / `benches/bfs_vs_dfs`).

Everything in that table is **statically resolvable at wiring time**. The layer
each node lands in (`graph.rs:774`, `layer = max(upstream.layer + 1)`) is the
schedule generated code bakes in. Active-vs-passive edges and the
"`Ok(false)` suppresses downstream" convention become compile-time control flow.

## 3. Non-goals (v0)

- No runtime `graph.to_rust()` — codegen is compile-time only.
- No `dynamic-graph` support (`add_upstream` / `remove_node`).
- No inlining of IO/async adapter internals — they stay behind the channel seam.
- Not a replacement for the interpreter — it is a second, opt-in back-end.

## 4. Compilation model

### 4.1 Front-end: `pipeline! { … }`

A proc-macro mirrors the fluent API but keeps closures as tokens:

```rust
let p = pipeline! {
    let ticks   = ticker(Duration::from_nanos(100));
    let n       = ticks.count();                 // u64 running count
    let doubled = n.map(|c| c * 2);
    let big     = doubled.filter_value(|v| v % 3 == 0);
    let total   = big.fold(0u64, |acc, v| *acc += v);
    output total;
};
```

The macro lowers each `let` binding to a symbolic operator node (its own small
algebra — `Source`, `Map`, `Filter`, `BiMap`, `Fold`, `Scan`, `Count`, `Sink`
…), records the closures as token trees, and builds a compile-time DAG.

### 4.2 Middle: schedule

Topologically sort the DAG by the same layering rule the interpreter uses
(`graph.rs:774`); passive upstreams raise a node's layer too, so a passively-read
value is always computed earlier in the sweep. Classify each node:

- **stateful** (`Fold`, `Scan`, `Count`, `Delay`, feedback source) → gets a
  `struct` field that persists across cycles;
- **passively-read or `output`** → gets a field (its latest value must survive
  between cycles);
- **everything else** → a fused local, no storage.

### 4.3 Back-end: emit `struct State` + `fn cycle`

Each node becomes a guarded statement in layer order. "Did an active upstream
tick" becomes an SSA boolean; downstream firing is a plain `if`. This is
`node_ticked` + `mark_dirty` + `peek_value`, lowered to locals the optimizer
can inline, register-allocate, and vectorize.

## 5. Worked examples

### 5.1 Pure historical pipeline (fusion)

Source `pipeline!` from §4.1. A naïve lowering gives one field per node:

```rust
#[derive(Default)]
struct Pipeline { count: u64, doubled: u64, big: u64, total: u64 }
```

But only `count` (stateful) and `total` (stateful + `output`) must persist;
`doubled` and `big` are read only by their single same-cycle downstream, so they
**fuse into locals**:

```rust
#[derive(Default)]
struct Pipeline {
    count: u64,   // Count accumulator
    total: u64,   // Fold accumulator (also the output)
}

impl Pipeline {
    /// One engine tick. `ticker_fired` = did the 100ns source fire this tick.
    #[inline]
    fn cycle(&mut self, ticker_fired: bool) {
        if !ticker_fired { return; }          // the ticker is the only source

        self.count += 1;                       // n       = count()
        let doubled = self.count * 2;          // doubled = n.map(|c| c*2)      [local]
        if doubled % 3 == 0 {                  // big     = doubled.filter(...) [local]
            self.total += doubled;             // total   = big.fold(0, +=)
        }
    }

    fn total(&self) -> u64 { self.total }      // output
}
```

Compare with the interpreter for the same three inner nodes: 3× (`Rc::clone` +
vtable + `borrow_mut` + `peek_value` clone + `Box<dyn Fn>` call + dirty-set
push). The generated body is one add, one multiply, one branch, one add — no
heap, no dispatch.

Historical driver (deterministic replay, **no channel** — historical mode
forbids ready callbacks, `graph.rs:843`):

```rust
fn run_historical(cycles: u64) -> Pipeline {
    let mut p = Pipeline::default();
    let mut t = NanoTime::ZERO;
    for _ in 0..cycles {
        p.cycle(true);           // only source is the ticker → fires every tick
        t = t + 100;             // ticker period
    }
    p
}
```

When there are several timed sources, the driver merges their `TimeQueue`s into
one timeline and passes a per-source `fired` set to `cycle()` per timestamp
(same shape as §5.3).

### 5.2 Realtime pipeline with IO — the channel seam

```rust
let p = pipeline! {
    let quotes = source::<Quote>("md");                        // IO source
    let mid    = quotes.map(|q| (q.bid + q.ask) / 2.0);
    let signal = mid.fold(0.0, |ema, m| *ema = 0.9 * *ema + 0.1 * m);
    sink(signal, "signal");                                    // IO sink
    output signal;
};
```

The IO adapter is **unchanged**. Its background thread still holds a
`ReadyNotifier { node_index, sender }` (`graph.rs:163`), still writes decoded
values into a side buffer, and still calls `notify()` (a `sender.send(id)`) when
data lands. Generated code treats the source as an opaque root with two ops:
`drain_latest()` (was `peek_value()`) and the channel id.

```rust
struct Pipeline {
    ema: f64,                       // Fold accumulator + output
    src_md: SourceHandle<Quote>,    // owns buffer + background thread (unchanged adapter)
    sink_signal: SinkHandle<f64>,   // unchanged adapter
}

const SRC_MD: usize = 0;            // stable compile-time id used on the ready channel

impl Pipeline {
    #[inline]
    fn cycle(&mut self, fired: &SourceFired) {
        if fired.contains(SRC_MD) {
            let q = self.src_md.drain_latest();            // was peek_value()
            let mid = (q.bid + q.ask) / 2.0;               // map  (fused local)
            self.ema = 0.9 * self.ema + 0.1 * mid;         // fold
            self.sink_signal.publish(self.ema);            // sink (adapter write)
        }
    }

    fn signal(&self) -> f64 { self.ema }
}
```

Realtime driver — this **is** `wait_ready_callback` / `process_callbacks_realtime`
(`graph.rs:410`, `graph.rs:884`), specialized. Drain the whole channel before
running a single fused cycle, exactly as the interpreter does (`graph.rs:873`):

```rust
fn run_realtime(mut p: Pipeline, ready_rx: Receiver<usize>, timers: &mut TimeQueue<usize>) {
    let mut fired = SourceFired::new();
    loop {
        // ── block until a source is ready OR the next timer is due ──
        let now  = NanoTime::now();
        let wake = timers.next_time().unwrap_or(NanoTime::MAX);
        select! {
            recv(ready_rx) -> idx => { if let Ok(i) = idx { fired.set(i); } }
            default((wake - now).into()) => {}
        }
        while let Ok(i)   = ready_rx.try_recv()               { fired.set(i); }  // drain rest
        while let Some(i) = timers.pop_if_pending(NanoTime::now()) { fired.set(i); }

        p.cycle(&fired);          // one fused sweep over the union of fired sources
        fired.clear();
        // (termination bounds omitted)
    }
}
```

The channel is the *only* dynamic element, it runs at the external-event rate
(not the compute rate), and it is inherent to IO — so keeping it costs nothing on
the hot path.

### 5.3 General fused sweep — active/passive + SSA ticks

The general form, with two sources and a mixed-edge `bimap` (active `sa`,
passive `b`):

```rust
let p = pipeline! {
    let a   = source::<f64>("a");
    let b   = source::<f64>("b");
    let sa  = a.map(|x| x * 2.0);
    let mix = bimap(Active(sa), Passive(b), |s, bv| s + bv);   // triggers on sa only
    output mix;
};
```

```rust
struct Pipeline {
    a: f64, b: f64, mix: f64,          // b is a field: read passively across cycles
    src_a: SourceHandle<f64>,
    src_b: SourceHandle<f64>,
}
const SRC_A: usize = 0;
const SRC_B: usize = 1;

impl Pipeline {
    #[inline]
    fn cycle(&mut self, fired: &SourceFired) {
        // layer 0 — sources (a passive read still updates its field first)
        let a_ticked = fired.contains(SRC_A) && { self.a = self.src_a.drain_latest(); true };
        let b_ticked = fired.contains(SRC_B) && { self.b = self.src_b.drain_latest(); true };

        // layer 1 — sa = a.map(*2)   (active upstream a)
        let sa_ticked = a_ticked && { let sa = self.a * 2.0; /* fused into mix below */
                                      self.mix_input = sa; true };

        // layer 2 — mix = bimap(active sa, passive b)
        //   fires only when sa ticks; reads b's *current field* (passive edge)
        let _mix_ticked = sa_ticked && { self.mix = self.mix_input + self.b; true };

        // b_ticked deliberately does NOT propagate to mix — passive edge.
        let _ = b_ticked;
    }

    fn mix(&self) -> f64 { self.mix }
}
```

This makes the three semantic rules explicit as compile-time control flow:

- **active edge** → contributes to the downstream's `ticked` guard (`a_ticked`
  drives `sa_ticked` drives `mix`);
- **passive edge** → read the upstream's field, but do **not** trigger
  (`b_ticked` is discarded; `mix` reads `self.b`);
- **layer order** guarantees `self.b` is refreshed *before* `mix` reads it in any
  cycle where `b` also ticked, matching `peek_value` semantics exactly.

(For readability the `sa` fusion is shown via a `mix_input` field; in practice a
single-downstream value like `sa` stays a `let` local and folds directly into the
`mix` expression.)

## 6. What is and isn't compilable

| Category | v0 | Notes |
|---|---|---|
| map, filter, bimap, trimap, zip, reduce | ✅ | fuse into one body |
| fold, scan, count, accumulate | ✅ | struct fields |
| constant, ticker, timed sources | ✅ | drive the driver loop via a minimal `TimeQueue` |
| delay, scheduled callbacks | ✅ | need the side `TimeQueue`, not the full engine |
| IO sources / sinks (kafka, zmq, web…) | ✅ (as boundary) | channel + buffer unchanged; not inlined |
| feedback loops | v1 | "write field this cycle, read next" + the `+1ns` schedule (`feedback.rs:54`) |
| async/tokio adapters | v1 | runtime on the side, same channel seam |
| dynamic graph | ❌ | out of scope by decision |

Anything unsupported → a clear compile error (later: fall back to embedding the
interpreter for that subgraph).

## 7. Correctness strategy

The load-bearing safety mechanism is a **differential test**: run the same
pipeline through the interpreter (`RunMode::HistoricalFrom`) and through the
generated code over identical historical input, and assert byte-identical
output. Wire this into `benches/bfs_vs_dfs` as a fourth contender to also
quantify the speedup on the canonical fan-out graph.

Semantic invariants the generated code must preserve, each checked by the diff:

1. tick / no-tick propagation (`filter` returning `Ok(false)` stops downstream);
2. active vs passive triggering;
3. fan-out dedup (a shared node runs once per cycle even with multiple
   upstreams — `node_dirty`, `graph.rs:814`);
4. layer/topological ordering of reads;
5. feedback `+1ns` timing (when v1 lands).

## 8. Incremental plan

1. **`wingfoil-codegen` proc-macro crate.** Parse `pipeline! {}`, build the
   symbolic DAG, topo-sort, emit `struct` + `cycle()`.
2. **v0 operator set:** `constant`/`ticker`/`count` sources; `map`/`filter`/
   `bimap`/`fold`/`scan` ops; one channel-backed IO source + one sink.
3. **Differential test harness** (interpreter vs generated, historical).
4. **Benchmark** vs interpreter on `breadth_first` and map/filter chains.
5. **v1:** feedback, delay/timers polish, async adapters.

Worth prototyping first (cheaper, de-risks the ceiling): partial non-codegen
wins — generic (monomorphized) subgraph combinators instead of `Rc<dyn>`,
dropping `RefCell` on the single-threaded path, `enum`-dispatch instead of
`dyn`. If these recover most of the gap for real workloads, full codegen may not
be worth the second semantics to maintain.

## 9. Risks

- **Payoff is workload-specific** — near-zero for heavy-per-node or IO-bound
  graphs. Position as a targeted feature, not a default.
- **Second execution semantics** — feedback timing, tick propagation, and
  passive reads must match the interpreter exactly. The differential test is
  non-negotiable.
- **API ergonomics** — because transforms must be visible tokens, users write
  inside `pipeline! {}` rather than composing pre-built `Rc<dyn Stream>` values.
  That is a real API shift, not a drop-in `.compile()`.
- **Maintenance coupling** — every new operator needs a lowering rule in the
  macro or an explicit "unsupported" error.

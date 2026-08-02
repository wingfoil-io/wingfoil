//! Engine span instrumentation — the `instrument-*` features.
//!
//! Pins the spans the interpreted engine emits: one `run`, one `initialise`,
//! one `apply_nodes` per lifecycle phase, one `cycle` per engine cycle, and —
//! under `instrument-cycle-node` — one `cycle_node` per node execution, tagged
//! with the node's index and label. The set mirrors classic wingfoil's
//! (`wingfoil/src/graph.rs`), so a subscriber written against one engine reads
//! the other.
//!
//! Runs under `instrument-all` (which CI's `--all-features` build enables). The
//! subscriber is installed with [`tracing::subscriber::with_default`] rather
//! than as a global default, so the recorder is scoped to this thread and the
//! tests stay parallel-safe.
#![cfg(feature = "instrument-all")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tracing::field::{Field, Visit};
use tracing::span::Attributes;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::interp::Dispatch;
use wingfoil_next::prelude::*;

/// One recorded span: its name plus the fields it was created with.
#[derive(Clone, Debug)]
struct Span {
    name: &'static str,
    fields: Vec<(String, String)>,
}

impl Span {
    /// The value of `key`, if the span carries it.
    fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// A `tracing` layer that appends every span created while it is installed.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<Span>>>);

impl Recorder {
    fn spans(&self) -> Vec<Span> {
        self.0.lock().expect("recorder mutex poisoned").clone()
    }

    /// Every span named `name`, in creation order.
    fn named(&self, name: &str) -> Vec<Span> {
        self.spans()
            .into_iter()
            .filter(|s| s.name == name)
            .collect()
    }
}

impl<S: tracing::Subscriber> Layer<S> for Recorder {
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &tracing::Id, _ctx: Context<'_, S>) {
        let mut fields = FieldGrab(Vec::new());
        attrs.record(&mut fields);
        self.0.lock().expect("recorder mutex poisoned").push(Span {
            name: attrs.metadata().name(),
            fields: fields.0,
        });
    }
}

/// Collects a span's construction-time fields as `(name, value)` strings.
struct FieldGrab(Vec<(String, String)>);

impl Visit for FieldGrab {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push((field.name().to_string(), value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .push((field.name().to_string(), format!("{value:?}")));
    }
}

/// Wire and run a three-cycle counter under `dispatch`, returning everything
/// the engine emitted. Both `build()` and `run()` happen inside the subscriber
/// scope so the `initialise` span is captured too.
fn record(dispatch: Dispatch) -> Recorder {
    let recorder = Recorder::default();
    let subscriber = tracing_subscriber::registry().with(recorder.clone());
    tracing::subscriber::with_default(subscriber, || {
        let g = GraphBuilder::new();
        let out = g.ticker(Duration::from_secs(1)).count();
        let states = out.accumulate();
        let mut runner = g.build().with_dispatch(dispatch);
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
            .unwrap();
        // The instrumentation must not disturb the values the graph produces.
        assert_eq!(runner.value(&states), vec![1u64, 2, 3]);
    });
    recorder
}

/// `instrument-run` wraps the whole `start → cycles → stop → teardown`
/// lifecycle in exactly one span.
#[test]
fn run_emits_one_span() {
    assert_eq!(record(Dispatch::Sparse).named("run").len(), 1);
}

/// `instrument-initialise` wraps `Builder::build` — next's counterpart of
/// classic's `Graph::initialise`, and named after it so the two engines read
/// alike.
#[test]
fn build_emits_the_initialise_span() {
    assert_eq!(record(Dispatch::Sparse).named("initialise").len(), 1);
}

/// `instrument-apply-nodes` wraps each lifecycle phase applied across all
/// nodes, recording the phase in `desc` (classic's field name). Next has no
/// separate `setup` phase — its ops are constructed at wiring time — so three
/// phases are emitted where classic emits four.
#[test]
fn each_lifecycle_phase_emits_an_apply_nodes_span() {
    let phases: Vec<String> = record(Dispatch::Sparse)
        .named("apply_nodes")
        .iter()
        .map(|s| s.field("desc").unwrap_or_default().to_string())
        .collect();
    assert_eq!(phases, vec!["start", "stop", "teardown"]);
}

/// `instrument-cycle` emits one span per engine cycle — three for
/// `RunFor::Cycles(3)`.
#[test]
fn each_cycle_emits_a_cycle_span() {
    assert_eq!(record(Dispatch::Sparse).named("cycle").len(), 3);
}

/// `instrument-cycle-node` emits one span per node execution, carrying the
/// node's dispatch index and its label. The graph is `ticker → count →
/// accumulate`, so each of the three cycles fires all three nodes.
#[test]
fn each_node_execution_emits_a_cycle_node_span() {
    let recorder = record(Dispatch::Sparse);
    let spans = recorder.named("cycle_node");
    assert_eq!(spans.len(), 9, "3 nodes x 3 cycles");

    // Every span identifies which node ran.
    for span in &spans {
        assert!(span.field("index").is_some(), "missing index: {span:?}");
        let node = span.field("node").expect("missing node label");
        assert!(!node.is_empty(), "empty node label");
    }

    // The first cycle walks the graph in dispatch (layer, index) order.
    let first_cycle: Vec<&str> = spans[..3]
        .iter()
        .map(|s| s.field("index").unwrap())
        .collect();
    assert_eq!(first_cycle, vec!["0", "1", "2"]);
}

/// The full-sweep reference oracle emits the same spans as the sparse
/// dispatcher — so instrumentation cannot be used to tell the two apart, just
/// as results cannot.
#[test]
fn full_sweep_emits_the_same_spans_as_sparse() {
    let sweep = record(Dispatch::FullSweep);
    assert_eq!(sweep.named("run").len(), 1);
    assert_eq!(sweep.named("initialise").len(), 1);
    assert_eq!(sweep.named("apply_nodes").len(), 3);
    assert_eq!(sweep.named("cycle").len(), 3);
    // The oracle visits *every* node every cycle, but only spans the ones it
    // actually cycles — the same three per cycle the sparse drain fires.
    assert_eq!(sweep.named("cycle_node").len(), 9);
}

/// Spans nest the way the engine does: `cycle` and the `apply_nodes` phases sit
/// inside `run`, and `cycle_node` inside `cycle`. Checked through the
/// subscriber's own parent tracking rather than by re-reading the code.
#[test]
fn spans_nest_run_cycle_node() {
    let recorder = Recorder::default();
    let parents = Arc::new(Mutex::new(
        Vec::<(&'static str, Option<&'static str>)>::new(),
    ));
    let layer = ParentRecorder(parents.clone());
    let subscriber = tracing_subscriber::registry()
        .with(recorder.clone())
        .with(layer);
    tracing::subscriber::with_default(subscriber, || {
        let g = GraphBuilder::new();
        let _out = g.ticker(Duration::from_secs(1)).count();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
    });

    let parents = parents.lock().expect("parents mutex poisoned").clone();
    let parent_of = |name: &str| -> Option<&'static str> {
        parents
            .iter()
            .find(|(n, _)| *n == name)
            .and_then(|(_, p)| *p)
    };
    assert_eq!(parent_of("run"), None, "`run` is a root span");
    assert_eq!(parent_of("apply_nodes"), Some("run"));
    assert_eq!(parent_of("cycle"), Some("run"));
    assert_eq!(parent_of("cycle_node"), Some("cycle"));
}

/// Records `(span name, parent span name)` for the nesting test.
struct ParentRecorder(Arc<Mutex<Vec<(&'static str, Option<&'static str>)>>>);

impl<S> Layer<S> for ParentRecorder
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &tracing::Id, ctx: Context<'_, S>) {
        // `current_span` is the span that was entered when this one was
        // created — its parent, since the engine's spans are all entered
        // in-place (RAII guards / `#[instrument]`).
        let parent = ctx.lookup_current().map(|s| s.name());
        self.0
            .lock()
            .expect("parents mutex poisoned")
            .push((attrs.metadata().name(), parent));
    }
}

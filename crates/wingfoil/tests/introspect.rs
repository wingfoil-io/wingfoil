//! Graph introspection against *real* wired graphs — the unit tests in
//! `src/introspect.rs` cover the renderers over hand-built snapshots, this
//! covers what the engine actually extracts from a graph the fluent API wired.

use std::time::Duration;

use wingfoil::introspect::{EdgeKind, GraphSnapshot};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

/// A ticker feeding a `count` feeding a `map` — the smallest chain with a
/// scheduling source and two combinators.
fn linear() -> GraphSnapshot {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_millis(10)).count();
    let _doubled = count.map(|n: &u64| n * 2);
    g.snapshot()
}

#[test]
fn snapshot_captures_a_linear_chain() {
    let snap = linear();
    assert_eq!(snap.node_count(), 3);
    assert_eq!(snap.edge_count(), 2);

    // Wiring order is a topological order, so indices run source-first.
    assert_eq!(snap.nodes[0].index, 0);
    assert!(snap.nodes[0].is_source());
    assert_eq!(snap.nodes[1].active_ups, vec![0]);
    assert_eq!(snap.nodes[2].active_ups, vec![1]);

    assert_eq!(snap.sources().count(), 1);
    assert_eq!(snap.sinks().map(|n| n.index).collect::<Vec<_>>(), vec![2]);
}

#[test]
fn a_ticker_is_labelled_and_declares_that_it_schedules() {
    let snap = linear();
    let source = &snap.nodes[0];
    // The label is the shortened `type_name` of the op.
    assert!(
        source.label.contains("Ticker"),
        "unexpected source label: {}",
        source.label
    );
    assert!(source.activation.schedules);
    assert_eq!(source.activation_tag(), Some("schedules"));
    // Downstream combinators are activated purely by an upstream tick.
    assert_eq!(snap.nodes[1].activation_tag(), None);
}

/// The distinction the whole surface exists for: `sample` reads its data leg
/// passively and is triggered by the clock leg.
#[test]
fn sample_records_a_passive_edge() {
    let g = GraphBuilder::new();
    let fast = g.ticker(Duration::from_millis(1)).count();
    let slow = g.ticker(Duration::from_millis(10));
    let _sampled = fast.sample(&slow);
    let snap = g.snapshot();

    let edges = snap.edges();
    let passive: Vec<_> = edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Passive)
        .collect();
    assert_eq!(
        passive.len(),
        1,
        "expected exactly one passive edge, got {edges:?}"
    );

    // The passive edge feeds the sample node, and it comes from the data leg
    // (`fast`), not the trigger.
    let sampled_idx = passive[0].to;
    assert_eq!(passive[0].from, fast.handle().index());
    assert!(
        snap.nodes[sampled_idx]
            .passive_ups
            .contains(&passive[0].from)
    );

    // ...while the trigger leg activates it.
    assert!(
        snap.nodes[sampled_idx]
            .active_ups
            .contains(&slow.handle().index())
    );
}

#[test]
fn passive_edges_render_differently_in_every_visual_format() {
    let g = GraphBuilder::new();
    let fast = g.ticker(Duration::from_millis(1)).count();
    let slow = g.ticker(Duration::from_millis(10));
    let _sampled = fast.sample(&slow);
    let snap = g.snapshot();

    let dot = snap.to_dot();
    assert!(dot.starts_with("digraph wingfoil {"), "{dot}");
    assert!(dot.contains("style=dashed"), "{dot}");
    assert!(dot.ends_with("}\n"), "{dot}");

    let mermaid = snap.to_mermaid();
    assert!(mermaid.starts_with("flowchart LR\n"), "{mermaid}");
    assert!(mermaid.contains("-.->|passive|"), "{mermaid}");
    assert!(mermaid.contains("-->"), "{mermaid}");

    // Text names both, with distinct arrows.
    let text = snap.to_text();
    assert!(text.contains(" <- "), "{text}");
    assert!(text.contains(" <~ "), "{text}");

    // GML cannot express the distinction — that is exactly why the other
    // formats exist — but it must still emit every edge.
    let gml = snap.to_gml();
    assert_eq!(gml.matches("edge [").count(), snap.edge_count());
    assert_eq!(gml.matches("node [").count(), snap.node_count());
}

#[test]
fn display_is_the_text_form() {
    let snap = linear();
    assert_eq!(format!("{snap}"), snap.to_text());
    assert!(
        snap.to_text()
            .starts_with("wingfoil graph: 3 nodes, 2 edges")
    );
}

/// The hand-written `to_json` must agree with what `serde` derives, so a
/// consumer can deserialize it straight back.
#[test]
fn json_round_trips_through_serde() {
    let snap = linear();
    let json = snap.to_json();

    let back: GraphSnapshot =
        serde_json::from_str(&json).expect("to_json output must parse as a GraphSnapshot");
    assert_eq!(back, snap);

    // ...and it is byte-identical to what serde itself would write, which is
    // what stops the hand-written writer drifting from the derive.
    let via_serde = serde_json::to_string(&snap).expect("snapshot serializes");
    assert_eq!(json, via_serde);
}

#[test]
fn runner_snapshot_matches_the_builder_and_survives_a_run() {
    let g = GraphBuilder::new();
    let count = g.ticker(Duration::from_millis(10)).count();
    let _doubled = count.map(|n: &u64| n * 2);
    let before = g.snapshot();

    let mut runner = g.build();
    assert_eq!(runner.snapshot(), before);

    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(5))
        .expect("run");

    // Topology is structural: running does not change it.
    assert_eq!(runner.snapshot(), before);
}

#[test]
fn an_empty_graph_snapshots_cleanly() {
    let g = GraphBuilder::new();
    let snap = g.snapshot();
    assert_eq!(snap.node_count(), 0);
    assert_eq!(snap.edge_count(), 0);
    assert_eq!(
        snap.to_dot(),
        "digraph wingfoil {\n    rankdir=LR;\n    node [shape=box, style=rounded, fontname=\"Helvetica\"];\n    edge [fontname=\"Helvetica\", fontsize=10];\n}\n"
    );
    assert_eq!(snap.to_mermaid(), "flowchart LR\n");
    assert_eq!(snap.to_json(), "{\"nodes\":[]}");
}

/// Snapshotting does not consume the builder, so it can be taken repeatedly
/// while wiring — the property that makes it usable as a debugging aid.
#[test]
fn snapshot_does_not_consume_the_builder() {
    let g = GraphBuilder::new();
    let ticker = g.ticker(Duration::from_millis(10));
    assert_eq!(g.snapshot().node_count(), 1);
    let count = ticker.count();
    assert_eq!(g.snapshot().node_count(), 2);
    let _ = count.map(|n: &u64| n * 2);
    assert_eq!(g.snapshot().node_count(), 3);

    // And the graph still builds and runs afterwards.
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .expect("run");
}

/// `Runner::snapshot` documents that it reflects nodes spliced in at runtime
/// and flags removed ones — the one claim the renderer unit tests cannot make,
/// since they build their tombstones by hand. This pins the *engine* side: that
/// `removed` stays index-aligned with the node table across an append and a
/// removal.
///
/// Mirrors the shape of `removed_node_stops_firing_and_value_freezes` in
/// `tests/dynamic_graph.rs`: a `fold` appended at cycle 1, removed at cycle 4.
#[test]
#[cfg(feature = "dynamic-graph")]
fn runner_snapshot_reflects_runtime_additions_and_removals() {
    let g = GraphBuilder::new();
    let src = g.ticker(Duration::from_nanos(1)).count().handle();
    let mut runner = g.build();
    assert_eq!(runner.snapshot().node_count(), 2);

    let mut appended = None;
    runner
        .run_dynamic(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(8),
            |ext, cycle| {
                if cycle == 1 {
                    appended = Some(ext.fold(src, 0u64, |acc, _| *acc += 1));
                }
                if cycle == 4 {
                    ext.remove(appended.expect("appended at cycle 1"))?;
                }
                Ok(())
            },
        )
        .expect("run");

    let snap = runner.snapshot();
    let appended = appended.expect("appended at cycle 1").index();

    // The spliced-in node is present in the data, and flagged.
    assert_eq!(snap.node_count(), 3);
    assert!(snap.nodes[appended].removed);
    assert!(snap.to_json().contains("\"removed\":true"));

    // ...but omitted from `live()` and from every rendered format, and it
    // contributes no edge — removal unwires it from its upstream, so nothing
    // renders a dangling reference to it.
    assert_eq!(snap.live().count(), 2);
    assert_eq!(snap.edge_count(), 1);
    assert!(!snap.to_text().contains(&format!("[{appended}]")), "{snap}");
    assert_eq!(snap.to_gml().matches("node [").count(), 2);
}

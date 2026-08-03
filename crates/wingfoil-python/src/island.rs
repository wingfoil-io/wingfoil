//! A **compiled island** exposed to Python — the one execution path that is not
//! interpreted.
//!
//! `nitro!` expands a single wiring into `interpreted()` / `compiled()` /
//! `nested()`. The island form is `nested()`: the whole sub-graph collapses to
//! one node in the outer graph, and its interior is monomorphized straight-line
//! code — the same code `compiled()` emits — instead of a dyn call per node.
//!
//! `nested()`'s signature is `(&GraphBuilder, &Stream<In>…) -> Stream<Out>`,
//! which is exactly a `#[pygraph]` wiring fn that takes the builder. So exposing
//! an island needs no island-specific machinery: Python wires *around* it
//! dynamically while the interior runs compiled — the "compiled interiors,
//! dynamic wiring" shape, rather than the whole graph being one or the other.
//!
//! This lives in its own module because `nitro!` requires the fluent `Stream`
//! and `GraphBuilder` unqualified, and `python.rs` shadows `Stream` with the
//! pyclass of the same name.

use wingfoil::prelude::*;

use crate::pygraph;

wingfoil::nitro! {
    fn hot_interior(_g: &GraphBuilder, input: &Stream<f64>) -> Stream<f64> {
        let doubled = input.map(|x: &f64| x * 2.0);
        let shifted = doubled.map(|x: &f64| x + 1.0);
        let squared = shifted.map(|x: &f64| x * x);
        squared
    }
}

/// A compiled sub-graph as one Python call: `compiled_island(graph, stream)`.
///
/// The interior (`x * 2`, `+ 1`, squared) is monomorphized straight-line code
/// mounted as a single node of the caller's interpreted graph — "compiled
/// interiors, dynamic wiring". `interpreted_twin` is the same wiring through
/// the ordinary fluent layer, for comparison.
#[pygraph(name = compiled_island)]
fn build_compiled_island(g: &GraphBuilder, input: &Stream<f64>) -> Stream<f64> {
    hot_interior::nested(g, input)
}

/// The same wiring through the plain fluent layer, so a test can assert the
/// island produces identical values and tick times. Kept beside the `nitro!`
/// above rather than derived from it — if the two ever disagree, that is the
/// island's semantics drifting, which is exactly what the parity test is for.
#[pygraph(name = interpreted_twin)]
fn build_interpreted_twin(input: &Stream<f64>) -> Stream<f64> {
    input
        .map(|x: &f64| x * 2.0)
        .map(|x: &f64| x + 1.0)
        .map(|x: &f64| x * x)
}

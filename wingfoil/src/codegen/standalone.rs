//! Standalone (fully monomorphized) code generation — "phase B2".
//!
//! [`generate_standalone`] emits a runner with **no executor and no nodes at
//! all**: node state becomes monomorphized locals, node semantics are
//! re-implemented inline, and the user's closures arrive through a generated
//! `Inputs` struct as generic parameters — so rustc inlines their bodies into
//! the schedule. Only the [`Kernel`](super::Kernel) (clock + scheduled
//! callbacks + run bounds) remains at runtime.
//!
//! The original wiring is used **only to generate** the file; at runtime the
//! `Inputs` struct is the source of truth for closures and configuration
//! (tick intervals, constant values). There is consequently no topology
//! fingerprint guard — regenerate the file when the wiring changes.
//!
//! # Restrictions
//!
//! Every node in the graph must be one of: `ticker`, `constant`, `sample`,
//! `map` (incl. `not`), `filter`, `fold` (incl. `reduce` / `count` /
//! `accumulate`), `merge` — and every value type must be nameable in
//! generated source (primitives, `String`, `Vec`, `Option`, `NanoTime`,
//! `ValueAt`). Anything else makes [`generate_standalone`] return an error
//! naming the offending nodes; use [`generate`](super::generate) (which
//! falls back to dynamic dispatch) for such graphs.

use std::fmt::Write as _;
use std::rc::Rc;

use super::{NodeInfo, NodeKind, node_infos, topology_table, type_is_nameable};
use crate::graph::Graph;
use crate::types::{NanoTime, Node};
use crate::{RunFor, RunMode};

use super::Result;

/// Per-node emission facts derived from [`NodeInfo`] for the standalone
/// generator.
struct Slot {
    /// Source string of the node's output value type, e.g. `Vec<String>`.
    /// `None` for `TickNode` (a node, not a stream).
    value_type: Option<String>,
    /// Closure generic parameter (`F{i}`) and its trait bound, for map/fold.
    closure_bound: Option<String>,
    /// `Inputs` field declaration, e.g. `pub map_4: F4`.
    input_field: Option<String>,
}

fn validate_and_slot(index: usize, info: &NodeInfo) -> Result<Slot> {
    let unsupported = |what: &str| {
        anyhow::anyhow!(
            "generate_standalone cannot compile node [{index:02}] {}: {what}. Supported kinds: \
             ticker, constant, sample, map, filter, fold, merge — with nameable value types. Use \
             wingfoil::codegen::generate (dynamic-dispatch fallback) for this graph.",
            info.short
        )
    };
    for g in &info.generics {
        if !type_is_nameable(g) {
            return Err(unsupported(&format!("value type `{g}` is not nameable")));
        }
    }
    let expect_shape = |ok: bool| -> Result<()> {
        if ok {
            Ok(())
        } else {
            Err(unsupported("unexpected upstream shape"))
        }
    };
    let generic = |n: usize| -> Result<String> {
        info.generics
            .get(n)
            .cloned()
            .ok_or_else(|| unsupported("missing generic argument"))
    };
    let slot = match info.kind {
        NodeKind::Tick => {
            expect_shape(info.actives.is_empty() && info.passives.is_empty())?;
            Slot {
                value_type: None,
                closure_bound: None,
                input_field: Some(format!("pub tick_{index}: NanoTime")),
            }
        }
        NodeKind::Constant => {
            expect_shape(info.actives.is_empty() && info.passives.is_empty())?;
            let t = generic(0)?;
            Slot {
                value_type: Some(t.clone()),
                closure_bound: None,
                input_field: Some(format!("pub constant_{index}: {t}")),
            }
        }
        NodeKind::Sample => {
            expect_shape(info.actives.len() == 1 && info.passives.len() == 1)?;
            Slot {
                value_type: Some(generic(0)?),
                closure_bound: None,
                input_field: None,
            }
        }
        NodeKind::Map => {
            expect_shape(info.actives.len() == 1)?;
            let (input, output) = (generic(0)?, generic(1)?);
            Slot {
                value_type: Some(output.clone()),
                closure_bound: Some(format!("F{index}: FnMut({input}) -> {output}")),
                input_field: Some(format!("pub map_{index}: F{index}")),
            }
        }
        NodeKind::Filter => {
            expect_shape(info.actives.len() == 2)?;
            Slot {
                value_type: Some(generic(0)?),
                closure_bound: None,
                input_field: None,
            }
        }
        NodeKind::Fold => {
            expect_shape(info.actives.len() == 1)?;
            let (input, output) = (generic(0)?, generic(1)?);
            Slot {
                value_type: Some(output.clone()),
                closure_bound: Some(format!("F{index}: FnMut(&mut {output}, {input})")),
                input_field: Some(format!("pub fold_{index}: F{index}")),
            }
        }
        NodeKind::Merge => {
            expect_shape(!info.actives.is_empty())?;
            Slot {
                value_type: Some(generic(0)?),
                closure_bound: None,
                input_field: None,
            }
        }
        NodeKind::Opaque => return Err(unsupported("kind is not supported")),
    };
    Ok(slot)
}

/// The cycle body for one node, mirroring the corresponding
/// `MutableNode::cycle`. With `as_statement = false` the lines form a block
/// expression yielding whether the node ticked (for `let ticked_i = cond &&
/// { ... };`); with `as_statement = true` they form plain statements for
/// nodes whose tick nobody consumes (`if cond { ... }`). `None` for nodes
/// with nothing to execute (a constant just ticks).
fn body_lines(index: usize, info: &NodeInfo, as_statement: bool) -> Option<Vec<String>> {
    let src = |ix: usize| format!("v{ix}.clone()");
    let with_tick = |mut lines: Vec<String>, tick_expr: String| {
        if !as_statement {
            lines.push(tick_expr);
        }
        Some(lines)
    };
    match info.kind {
        NodeKind::Tick => with_tick(
            vec![
                format!("let next = match tick_at_{index} {{"),
                format!("    Some(t) => t + inputs.tick_{index},"),
                format!("    None => k.time() + inputs.tick_{index},"),
                "};".to_string(),
                format!("tick_at_{index} = Some(next);"),
                format!("k.schedule({index}, next);"),
            ],
            "true".to_string(),
        ),
        NodeKind::Constant => None,
        NodeKind::Sample => with_tick(
            vec![format!("v{index} = {};", src(info.passives[0]))],
            "true".to_string(),
        ),
        NodeKind::Map => with_tick(
            vec![format!(
                "v{index} = (inputs.map_{index})({});",
                src(info.actives[0])
            )],
            "true".to_string(),
        ),
        NodeKind::Filter => {
            let (source, condition) = (info.actives[0], info.actives[1]);
            with_tick(
                vec![format!("if v{condition} {{ v{index} = {}; }}", src(source))],
                format!("v{condition}"),
            )
        }
        NodeKind::Fold => with_tick(
            vec![format!(
                "(inputs.fold_{index})(&mut v{index}, {});",
                src(info.actives[0])
            )],
            "true".to_string(),
        ),
        NodeKind::Merge => {
            let mut lines = Vec::new();
            for (pos, &u) in info.actives.iter().enumerate() {
                let head = if pos == 0 { "if" } else { "} else if" };
                lines.push(format!("{head} ticked_{u} {{"));
                lines.push(format!("    v{index} = {};", src(u)));
                if !as_statement {
                    lines.push("    true".to_string());
                }
            }
            if as_statement {
                lines.push("}".to_string());
            } else {
                lines.push("} else {".to_string());
                lines.push("    false".to_string());
                lines.push("}".to_string());
            }
            Some(lines)
        }
        NodeKind::Opaque => None,
    }
}

/// Generate a fully monomorphized standalone runner for the graph reachable
/// from `roots`. See the [module docs](self) for semantics and restrictions.
///
/// The emitted file exposes `pub struct Inputs<...>` (closures and
/// configuration, in node-index order) and `pub fn run(inputs, run_mode,
/// run_for)` returning the final value(s) of the root node(s).
pub fn generate_standalone(roots: Vec<Rc<dyn Node>>, function_name: &str) -> Result<String> {
    let mut graph = Graph::new(
        roots.clone(),
        RunMode::HistoricalFrom(NanoTime::ZERO),
        RunFor::Forever,
    );
    if let Some(e) = graph.take_wiring_error() {
        return Err(e);
    }
    let infos = node_infos(&graph);
    let n = infos.len();
    let slots: Vec<Slot> = infos
        .iter()
        .enumerate()
        .map(|(i, info)| validate_and_slot(i, info))
        .collect::<Result<_>>()?;

    // Root nodes (in caller order, deduplicated) whose value slots become the
    // return value.
    let mut root_ixs: Vec<usize> = Vec::new();
    for root in &roots {
        let ix = graph
            .state
            .node_index(root.clone())
            .expect("root wired by Graph::new above");
        if !root_ixs.contains(&ix) && slots[ix].value_type.is_some() {
            root_ixs.push(ix);
        }
    }
    let return_type = match root_ixs.as_slice() {
        [] => "()".to_string(),
        ixs => format!(
            "({},)",
            ixs.iter()
                .map(|&ix| slots[ix].value_type.clone().expect("filtered above"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let return_expr = match root_ixs.as_slice() {
        [] => "()".to_string(),
        ixs => format!(
            "({},)",
            ixs.iter()
                .map(|&ix| format!("v{ix}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };

    let closure_params: Vec<String> = slots
        .iter()
        .enumerate()
        .filter(|(_, s)| s.closure_bound.is_some())
        .map(|(i, _)| format!("F{i}"))
        .collect();
    let generics = if closure_params.is_empty() {
        String::new()
    } else {
        format!("<{}>", closure_params.join(", "))
    };

    let mut out = String::new();
    let w = &mut out;
    writeln!(
        w,
        "// @generated by wingfoil::codegen (standalone). DO NOT EDIT."
    )?;
    writeln!(w, "//")?;
    writeln!(
        w,
        "// Fully monomorphized standalone runner for a wingfoil graph with {n} nodes."
    )?;
    writeln!(
        w,
        "// Node state lives in locals, node semantics are inlined, and closures come"
    )?;
    writeln!(
        w,
        "// from `Inputs` — the compiler inlines their bodies into the schedule. Only"
    )?;
    writeln!(
        w,
        "// the wingfoil `Kernel` (clock + scheduled callbacks) remains at runtime."
    )?;
    writeln!(w, "//")?;
    writeln!(
        w,
        "// `Inputs` is the source of truth for closures and configuration; regenerate"
    )?;
    writeln!(w, "// this file whenever the wiring changes.")?;
    writeln!(w, "//")?;
    for line in topology_table(&graph).lines() {
        writeln!(w, "// {line}")?;
    }
    writeln!(w)?;
    writeln!(w, "use wingfoil::codegen::Kernel;")?;
    writeln!(w, "use wingfoil::{{NanoTime, RunFor, RunMode}};")?;
    writeln!(w)?;
    writeln!(
        w,
        "/// Closures and configuration for the compiled graph, in node-index order."
    )?;
    writeln!(w, "pub struct Inputs{generics} {{")?;
    for slot in &slots {
        if let Some(field) = &slot.input_field {
            writeln!(w, "    {field},")?;
        }
    }
    writeln!(w, "}}")?;
    writeln!(w)?;
    writeln!(
        w,
        "/// Runs the compiled graph, returning the final value(s) of the root node(s)."
    )?;
    // Value propagation is emitted as `.clone()` uniformly; the generator
    // does not track `Copy`-ness, and cloning a `Copy` type compiles to a
    // plain copy.
    writeln!(w, "#[allow(clippy::clone_on_copy)]")?;
    writeln!(w, "#[rustfmt::skip]")?;
    writeln!(w, "pub fn {function_name}{generics}(")?;
    // `mut` is only needed when closures are called through `inputs`.
    if closure_params.is_empty() {
        writeln!(w, "    inputs: Inputs,")?;
    } else {
        writeln!(w, "    mut inputs: Inputs{generics},")?;
    }
    writeln!(w, "    run_mode: RunMode,")?;
    writeln!(w, "    run_for: RunFor,")?;
    writeln!(w, ") -> {return_type}")?;
    if !closure_params.is_empty() {
        writeln!(w, "where")?;
        for slot in &slots {
            if let Some(bound) = &slot.closure_bound {
                writeln!(w, "    {bound},")?;
            }
        }
    }
    writeln!(w, "{{")?;
    writeln!(w, "    let mut k = Kernel::new(run_mode, run_for);")?;
    writeln!(w, "    let mut dirty = [false; {n}];")?;
    writeln!(w, "    // node state")?;
    for (i, (info, slot)) in infos.iter().zip(&slots).enumerate() {
        match info.kind {
            NodeKind::Tick => {
                writeln!(w, "    let mut tick_at_{i}: Option<NanoTime> = None;")?;
            }
            NodeKind::Constant => {
                let t = slot.value_type.as_deref().expect("constant has a slot");
                writeln!(w, "    let v{i}: {t} = inputs.constant_{i};")?;
            }
            _ => {
                if let Some(t) = &slot.value_type {
                    writeln!(w, "    let mut v{i}: {t} = Default::default();")?;
                }
            }
        }
    }
    writeln!(w, "    // start: sources schedule their first callback")?;
    for (i, info) in infos.iter().enumerate() {
        if matches!(info.kind, NodeKind::Tick | NodeKind::Constant) {
            writeln!(w, "    k.schedule({i}, k.start_time());")?;
        }
    }
    writeln!(w, "    while k.begin_cycle(&mut dirty) {{")?;
    for (i, info) in infos.iter().enumerate() {
        let mut cond_parts: Vec<String> =
            info.actives.iter().map(|u| format!("ticked_{u}")).collect();
        // The dirty check only exists for callback activation; drop it when
        // the node type provably never registers callbacks, so quiet
        // subgraphs stay on register-only logic.
        if super::can_receive_callbacks(&info.short) {
            cond_parts.push(format!("dirty[{i}]"));
        }
        let cond = cond_parts.join(" || ");
        let cond_grouped = if cond_parts.len() > 1 {
            format!("({cond})")
        } else {
            cond.clone()
        };
        writeln!(w, "        // [{i:02}] {}", info.short)?;
        match body_lines(i, info, !info.has_active_downstream) {
            None => {
                // Constant: ticks whenever dispatched, no work to do.
                if info.has_active_downstream {
                    writeln!(w, "        let ticked_{i} = {cond};")?;
                }
            }
            Some(body) => {
                if info.has_active_downstream {
                    writeln!(w, "        let ticked_{i} = {cond_grouped} && {{")?;
                } else {
                    writeln!(w, "        if {cond} {{")?;
                }
                for line in &body {
                    writeln!(w, "            {line}")?;
                }
                if info.has_active_downstream {
                    writeln!(w, "        }};")?;
                } else {
                    writeln!(w, "        }}")?;
                }
            }
        }
    }
    writeln!(w, "        k.end_cycle(&mut dirty);")?;
    writeln!(w, "    }}")?;
    writeln!(w, "    {return_expr}")?;
    writeln!(w, "}}")?;
    Ok(out)
}

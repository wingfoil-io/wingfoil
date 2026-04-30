use crate::queue::HashByRef;
use crate::queue::TimeQueue;
use crate::types::{NanoTime, Node};

use crossbeam::channel::{Receiver, SendError, Sender, select};
use lazy_static::lazy_static;
use std::cmp::{max, min};
use std::collections::HashMap;
#[cfg(feature = "dynamic-graph-beta")]
use std::collections::HashSet;
use std::fs::File;
use std::io::{Error, Write};
use std::path::Path;
use std::rc::Rc;
#[cfg(feature = "async")]
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use std::vec;

lazy_static! {
    static ref GRAPH_ID: Mutex<usize> = Mutex::new(0);
}

struct NodeData {
    node: Rc<dyn Node>,
    upstreams: Vec<(usize, bool)>,
    downstreams: Vec<(usize, bool)>,
    layer: usize,
    active: bool,
}

/// Whether the [Graph] should run RealTime or Historical mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RunMode {
    RealTime,
    HistoricalFrom(NanoTime),
}

impl RunMode {
    pub fn start_time(&self) -> NanoTime {
        match self {
            RunMode::RealTime => NanoTime::now(),
            RunMode::HistoricalFrom(start_time) => *start_time,
        }
    }
}

/// Defines how long the graph should run for.  Can be a
/// Duration, number of cycles or forever.
#[derive(Clone, Copy, Debug)]
pub enum RunFor {
    Duration(Duration),
    Cycles(u32),
    Forever,
}

impl RunFor {
    pub fn done(&self, cycle: u32, elapsed: NanoTime) -> bool {
        match self {
            RunFor::Cycles(cycles) => cycle > *cycles,
            RunFor::Duration(duration) => elapsed > NanoTime::from(*duration),
            RunFor::Forever => false,
        }
    }
}

fn average_duration(duration: Duration, n: u32) -> Duration {
    let avg_nanos = if n == 0 {
        0
    } else {
        duration.as_nanos() / n as u128
    };
    Duration::from_nanos(avg_nanos as u64)
}

/// A struct produced by [Graph] that can be used by a [Node]
/// to notify the [Graph] that it is required to be cycled
/// on the next engine cycle.   It is bound to the [Node]
/// that created it.
#[derive(Clone, Debug)]
pub(crate) struct ReadyNotifier {
    pub node_index: usize,
    pub sender: Sender<usize>,
}

impl ReadyNotifier {
    pub fn notify(&self) -> anyhow::Result<(), SendError<usize>> {
        self.sender.send(self.node_index)
    }
}

/// Maintains the parts of the graph state that is accessible to Nodes.
pub struct GraphState {
    time: NanoTime,
    /// Wall-clock timestamp of the start of the current engine cycle.
    /// Unlike [`time`], this is always a real wall-clock snap in both realtime
    /// and historical mode — used for latency measurement and perf telemetry.
    /// It is populated once per cycle (before nodes are dispatched) and
    /// explicitly does not feed business-logic decisions.
    wall_time: NanoTime,
    /// True until the first engine cycle completes; suppresses the
    /// strict-advance check so the very first cycle can fire at NanoTime::ZERO.
    first_cycle: bool,
    is_last_cycle: bool,
    stop_requested: bool,
    current_node_index: Option<usize>,
    scheduled_callbacks: TimeQueue<usize>,
    always_callbacks: Vec<usize>,
    node_to_index: HashMap<HashByRef<dyn Node>, usize>,
    node_ticked: Vec<bool>,
    #[cfg(feature = "async")]
    run_time: Option<Arc<tokio::runtime::Runtime>>,
    run_mode: RunMode,
    run_for: RunFor,
    ready_notifier: Sender<usize>,
    ready_callbacks: Receiver<usize>,
    start_time: NanoTime,
    id: usize,
    nodes: Vec<NodeData>,
    dirty_nodes_by_layer: Vec<Vec<usize>>,
    node_dirty: Vec<bool>,
    #[cfg(feature = "dynamic-graph-beta")]
    pending_additions: Vec<(Rc<dyn Node>, usize, bool, bool)>,
    #[cfg(feature = "dynamic-graph-beta")]
    pending_removals: Vec<Rc<dyn Node>>,
}

impl GraphState {
    pub fn new(run_mode: RunMode, run_for: RunFor, start_time: NanoTime) -> Self {
        let (ready_notifier, ready_callbacks) = crossbeam::channel::unbounded();
        let mut id = GRAPH_ID
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let slf = Self {
            time: NanoTime::ZERO,
            wall_time: NanoTime::ZERO,
            first_cycle: true,
            is_last_cycle: false,
            stop_requested: false,
            current_node_index: None,
            scheduled_callbacks: TimeQueue::new(),
            always_callbacks: Vec::new(),
            node_to_index: HashMap::new(),
            node_ticked: Vec::new(),
            #[cfg(feature = "async")]
            run_time: None,
            ready_notifier,
            run_mode,
            run_for,
            ready_callbacks,
            start_time,
            id: *id,
            nodes: Vec::new(),
            dirty_nodes_by_layer: Vec::new(),
            node_dirty: Vec::new(),
            #[cfg(feature = "dynamic-graph-beta")]
            pending_additions: Vec::new(),
            #[cfg(feature = "dynamic-graph-beta")]
            pending_removals: Vec::new(),
        };
        *id += 1;
        slf
    }

    /// The current engine time
    pub fn time(&self) -> NanoTime {
        self.time
    }

    /// Wall-clock time of the *start* of the current engine cycle.
    ///
    /// Same in both realtime and historical mode — always a wall-clock snap.
    /// Intended for latency stamping, cycle timing, and perf telemetry; never
    /// for business-logic decisions (which should use [`time`] for
    /// deterministic replay).
    ///
    /// Cost: one `u64` load. Snapped once per cycle.
    pub fn wall_time(&self) -> NanoTime {
        self.wall_time
    }

    /// Wall-clock time snapped fresh right now. Costs one `quanta` TSC read
    /// (~5-10 ns on x86). Use when you need intra-cycle resolution — i.e. to
    /// distinguish stages that run in the same engine cycle.
    pub fn wall_time_precise(&self) -> NanoTime {
        NanoTime::now()
    }

    /// The current engine time
    pub fn elapsed(&self) -> NanoTime {
        self.time - self.start_time
    }

    pub fn start_time(&self) -> NanoTime {
        self.start_time
    }

    pub(crate) fn ready_notifier(&self) -> anyhow::Result<ReadyNotifier> {
        let node_index = self
            .current_node_index
            .ok_or_else(|| anyhow::anyhow!("ready_notifier called outside node processing"))?;
        Ok(ReadyNotifier {
            node_index,
            sender: self.ready_notifier.clone(),
        })
    }

    #[cfg(feature = "async")]
    pub fn tokio_runtime(&mut self) -> anyhow::Result<Arc<tokio::runtime::Runtime>> {
        if let Some(rt) = &self.run_time {
            return Ok(rt.clone());
        }
        if tokio::runtime::Handle::try_current().is_ok() {
            anyhow::bail!(
                "wingfoil cannot be run from an async context (e.g. `#[tokio::main]`). \
                 Call graph.run() from a synchronous thread instead. \
                 Tip: std::thread::spawn(|| graph.run(...)).join().map_err(..)"
            );
        }
        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|e| anyhow::anyhow!("failed to build tokio runtime: {e}"))?,
        );
        self.run_time = Some(rt.clone());
        Ok(rt)
    }

    pub fn add_callback(&mut self, time: NanoTime) -> anyhow::Result<()> {
        let ix = self
            .current_node_index
            .ok_or_else(|| anyhow::anyhow!("add_callback called outside node processing"))?;
        self.add_callback_for_node(ix, time);
        Ok(())
    }

    pub(crate) fn current_node_id(&self) -> anyhow::Result<usize> {
        self.current_node_index
            .ok_or_else(|| anyhow::anyhow!("current_node_id called outside node processing"))
    }

    pub fn always_callback(&mut self) -> anyhow::Result<()> {
        let ix = self
            .current_node_index
            .ok_or_else(|| anyhow::anyhow!("always_callback called outside node processing"))?;
        self.always_callbacks.push(ix);
        Ok(())
    }

    pub fn is_last_cycle(&self) -> bool {
        self.is_last_cycle
    }

    /// Request early termination of the graph execution.
    /// This is useful for nodes like `limit` that need to stop the graph
    /// when they've finished producing values, even with RunFor::Forever.
    pub fn request_stop(&mut self) {
        self.stop_requested = true;
    }

    /// Returns true if node has ticked on the current engine cycle.
    /// Returns false (rather than panicking) for nodes not yet registered in the graph,
    /// which can happen when a node was just queued via `add_upstream` but not yet wired.
    pub fn ticked(&self, node: Rc<dyn Node>) -> bool {
        self.node_index(node)
            .map(|i| self.node_ticked[i])
            .unwrap_or(false)
    }

    /// Wire `upstream` (and its upstream subgraph) into the graph and register
    /// it as an upstream of the calling node. `is_active` controls whether it
    /// triggers the calling node on each tick (true) or is read-only (false).
    /// Processed at the end of the current cycle.
    ///
    /// If `recycle` is true, `add_callback(state.time() + 1ns)` is called on
    /// the new upstream after it is wired, scheduling it to fire at `t+1ns`.
    /// This lets the calling node catch the value that triggered the
    /// `add_upstream` call without waiting for the next source tick.
    #[cfg(feature = "dynamic-graph-beta")]
    pub fn add_upstream(
        &mut self,
        upstream: Rc<dyn Node>,
        is_active: bool,
        recycle: bool,
    ) -> anyhow::Result<()> {
        let caller_index = self
            .current_node_index
            .ok_or_else(|| anyhow::anyhow!("add_upstream called outside node processing"))?;
        self.pending_additions
            .push((upstream, caller_index, is_active, recycle));
        Ok(())
    }

    /// Deregister `node` at the end of the current cycle:
    /// unlinks it from all upstream downstream-lists and all downstream upstream-lists,
    /// then calls stop() + teardown().
    ///
    /// **Note on memory**: removed nodes are marked inactive but their entries in the
    /// internal index (`node_to_index`, `node_ticked`, `node_dirty`, `nodes`) are never
    /// freed. In long-running graphs that add and remove many nodes over time, these
    /// dead entries accumulate. This is a known limitation of the current implementation.
    #[cfg(feature = "dynamic-graph-beta")]
    pub fn remove_node(&mut self, node: Rc<dyn Node>) {
        self.pending_removals.push(node);
    }

    #[allow(dead_code)]
    /// Returns true if node has ticked on the current engine cycle
    pub(crate) fn node_index_ticked(&self, node_index: usize) -> bool {
        self.node_ticked[node_index]
    }

    fn has_scheduled_callbacks(&self) -> bool {
        !self.scheduled_callbacks.is_empty()
    }

    fn next_scheduled_time(&self) -> NanoTime {
        self.scheduled_callbacks
            .next_time()
            .unwrap_or(NanoTime::MAX)
    }

    pub(crate) fn add_callback_for_node(&mut self, node_index: usize, time: NanoTime) {
        self.scheduled_callbacks.push(node_index, time);
    }

    fn has_pending_scheduled_callbacks(&self) -> bool {
        self.scheduled_callbacks.pending(self.time)
    }

    fn wait_ready_callback(&mut self, end_time: NanoTime) -> Option<usize> {
        let now = NanoTime::now();
        if now > end_time {
            // might step in here if timeout on recv isnt 100%
            // accurate
            None
        } else {
            let timeout = u64::from(end_time - now);
            select! {
                recv(self.ready_callbacks) -> msg => {
                    msg.ok()
                },
                default(Duration::from_nanos(timeout)) => {
                    None
                }
            }
        }
    }

    pub fn node_index(&self, node: Rc<dyn Node>) -> Option<usize> {
        let key = HashByRef::new(node.clone());
        self.node_to_index.get(&key).copied()
    }

    fn reset(&mut self) {
        for i in self.node_ticked.iter_mut() {
            *i = false;
        }
    }

    fn push_node(&mut self, node: Rc<dyn Node>) {
        let index = self.node_ticked.len();
        self.node_ticked.push(false);
        //self.nodes.push(node.clone());
        self.node_to_index
            .insert(HashByRef::new(node.clone()), index);
    }

    fn seen(&self, node: Rc<dyn Node>) -> bool {
        self.node_to_index.contains_key(&HashByRef::new(node))
    }

    fn set_ticked(&mut self, index: usize) {
        self.node_ticked[index] = true;
    }

    pub fn run_mode(&self) -> RunMode {
        self.run_mode
    }

    pub fn run_for(&self) -> RunFor {
        self.run_for
    }

    pub fn log(&self, level: log::Level, msg: &str) {
        let Some(ix) = self.current_node_index else {
            return;
        };
        let id = self.id;
        #[cfg(not(feature = "tracing"))]
        if log_enabled!(level) {
            let type_name = self.nodes[ix].node.type_name();
            log!(target: &type_name, level, "[{id},{ix}]{msg}");
        }
        #[cfg(feature = "tracing")]
        if tracing_log_enabled!(level) {
            let type_name = self.nodes[ix].node.type_name();
            tracing_log!(level, node = %type_name, "[{id},{ix}]{msg}");
        }
    }

    pub(crate) fn mark_dirty(&mut self, index: usize) {
        if !self.node_dirty[index] {
            let layer = self.nodes[index].layer;
            self.dirty_nodes_by_layer[layer].push(index);
            self.node_dirty[index] = true;
        }
    }
}

/// Engine for co-ordinating execution of [Node]s
pub struct Graph {
    pub(crate) state: GraphState,
}

impl Graph {
    pub fn new(root_nodes: Vec<Rc<dyn Node>>, run_mode: RunMode, run_for: RunFor) -> Graph {
        let start_time = run_mode.start_time();
        let state = GraphState::new(run_mode, run_for, start_time);
        let mut graph = Graph { state };
        graph.initialise(root_nodes);
        graph
    }

    #[cfg(feature = "async")]
    pub fn new_with(
        root_nodes: Vec<Rc<dyn Node>>,
        tokio_runtime: Arc<tokio::runtime::Runtime>,
        run_mode: RunMode,
        run_for: RunFor,
        start_time: NanoTime,
    ) -> Graph {
        let mut state = GraphState::new(run_mode, run_for, start_time);
        state.run_time = Some(tokio_runtime);
        let mut graph = Graph { state };
        graph.initialise(root_nodes);
        graph
    }

    pub(crate) fn setup_nodes(&mut self) -> anyhow::Result<()> {
        self.apply_nodes("setup", |node, state| node.setup(state))
    }

    pub(crate) fn start_nodes(&mut self) -> anyhow::Result<()> {
        self.apply_nodes("start", |node, state| node.start(state))
    }

    pub(crate) fn stop_nodes(&mut self) -> anyhow::Result<()> {
        self.apply_nodes("stop", |node, state| node.stop(state))
    }

    pub(crate) fn teardown_nodes(&mut self) -> anyhow::Result<()> {
        self.apply_nodes("teardown", |node, state| node.teardown(state))
    }

    #[cfg_attr(
        feature = "instrument-apply-nodes",
        tracing::instrument(skip(self, func))
    )]
    fn apply_nodes(
        &mut self,
        desc: &str,
        func: impl Fn(Rc<dyn Node>, &mut GraphState) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let timer = Instant::now();
        for ix in 0..self.state.nodes.len() {
            if !self.state.nodes[ix].active {
                continue;
            }
            let node = self.state.nodes[ix].node.clone();
            self.state.current_node_index = Some(ix);
            func(node, &mut self.state).map_err(|e| {
                let context = self.format_context(ix, 3);
                e.context(format!("Error during {desc} in node [{ix}]:\n{context}"))
            })?;
            self.state.current_node_index = None;
        }
        debug!(
            "graph {:?}, {:?} took {:?} for {:?} nodes",
            self.state.id,
            desc,
            timer.elapsed(),
            self.state.nodes.len()
        );
        //println!("*** {:}graph {:} {:} done", "   ".repeat(self.state.id), self.state.id, desc);
        Ok(())
    }

    fn resolve_start_end(
        &self,
        start_time: &mut NanoTime,
        end_time: &mut NanoTime,
        end_cycle: &mut u32,
        is_realtime: &mut bool,
    ) {
        *end_time = NanoTime::MAX; // can update after first tick
        *end_cycle = u32::MAX; // can update after first tick
        match self.state.run_mode() {
            RunMode::RealTime => {
                *is_realtime = true;
                *start_time = NanoTime::now();
            }
            RunMode::HistoricalFrom(t) => {
                *is_realtime = false;
                *start_time = t;
            }
        };
        match self.state.run_for {
            RunFor::Duration(duration) => {
                *end_time = *start_time + duration.as_nanos() as u64;
                debug!("end_time = {end_time}",);
            }
            RunFor::Cycles(cycle) => {
                *end_cycle = cycle;
                debug!("end_cycle = {end_cycle}",);
            }
            RunFor::Forever => {}
        }
    }

    pub(crate) fn run_nodes(&mut self) -> anyhow::Result<()> {
        //println!("*** {:}graph {:} run_nodes", "   ".repeat(self.state.id), self.state.id);
        let run_timer = Instant::now();
        let mut cycles: u32 = 0;
        let mut empty_cycles: u32 = 0;
        let mut end_time = NanoTime::MAX;
        let mut end_cycle = u32::MAX;
        let mut is_realtime = false;
        let mut start_time = NanoTime::ZERO;
        self.resolve_start_end(
            &mut start_time,
            &mut end_time,
            &mut end_cycle,
            &mut is_realtime,
        );
        self.state.start_time = start_time;
        loop {
            if self.state.is_last_cycle && (self.state.time >= end_time || cycles >= end_cycle) {
                debug!(
                    "Finished. {:}, {:}, {:}, {:}",
                    self.state.time >= end_time,
                    cycles >= end_cycle,
                    self.state.time,
                    end_time
                );
                break;
            }
            if !self.state.is_last_cycle && (cycles >= end_cycle - 1 || self.state.time >= end_time)
            {
                debug!("last cycle");
                self.state.is_last_cycle = true;
            }
            if is_realtime {
                let progressed = self.process_callbacks_realtime(end_time);
                if !progressed {
                    empty_cycles += 1;
                    continue;
                }
            } else {
                let progressed = self.process_callbacks_historical();
                if !progressed {
                    debug!("Terminating early.");
                    break;
                }
            }
            self.cycle()?;
            /*
            if cycles == 0 && !is_realtime {
                // first cycle
                if let RunFor::Duration(duration) = run_for {
                    end_time = self.state.time + duration.as_nanos();
                    debug!("end_time = {:}", end_time);
                }
            }
             */
            cycles += 1;
            debug!("cycles={cycles}");
            if self.state.stop_requested {
                debug!("Stop requested by node, terminating early.");
                break;
            }
        }
        let elapsed = run_timer.elapsed();
        debug!("{empty_cycles} empty cycles");
        debug!(
            "Completed {:} cycles  in {:?}. {:?} average.",
            cycles,
            run_timer.elapsed(),
            average_duration(elapsed, cycles)
        );
        //println!("*** {:}graph {:} run_nodes done", "   ".repeat(self.state.id), self.state.id);
        Ok(())
    }

    #[cfg_attr(feature = "instrument-run", tracing::instrument(skip_all))]
    pub fn run(&mut self) -> anyhow::Result<()> {
        self.setup_nodes()?;
        self.start_nodes()?;
        self.run_nodes()?;
        self.stop_nodes()?;
        self.teardown_nodes()?;
        Ok(())
    }

    #[cfg_attr(feature = "instrument-initialise", tracing::instrument(skip_all))]
    fn initialise(&mut self, root_nodes: Vec<Rc<dyn Node>>) -> &mut Graph {
        let timer = Instant::now();
        for node in root_nodes {
            if !self.state.seen(node.clone()) {
                self.initialise_node(&node);
            }
        }
        let max_layer = self.state.nodes.iter().map(|n| n.layer).max();
        for i in 0..self.state.nodes.len() {
            self.state.node_dirty.push(false);
            for j in 0..self.state.nodes[i].upstreams.len() {
                let (up_index, active) = self.state.nodes[i].upstreams[j];
                self.state.nodes[up_index].downstreams.push((i, active));
            }
        }
        if let Some(max_layer) = max_layer {
            for _ in 0..=max_layer {
                self.state.dirty_nodes_by_layer.push(vec![]);
            }
        }
        debug!(
            "{:} nodes wired in {:?}",
            self.state.nodes.len(),
            timer.elapsed()
        );
        self
    }

    fn initialise_upstreams(
        &mut self,
        upstreams: &[Rc<dyn Node>],
        is_active: bool,
        layer: &mut usize,
        upstream_indexes: &mut Vec<(usize, bool)>,
    ) {
        for upstream_node in upstreams {
            let upstream_index = self.initialise_node(upstream_node);
            upstream_indexes.push((upstream_index, is_active));
            *layer = max(*layer, self.state.nodes[upstream_index].layer + 1);
        }
    }

    fn initialise_node(&mut self, node: &Rc<dyn Node>) -> usize {
        // recursively crawl through graph defined by node
        // constructing NodeData wrapper for each node and pushing
        // onto self.nodes returns index of new NodeData in self.nodes
        if let Some(ix) = self.state.node_index(node.clone()) {
            ix
        } else {
            let mut layer = 0;
            let mut upstream_indexes = vec![];
            let upstreams = node.upstreams();
            self.initialise_upstreams(&upstreams.active, true, &mut layer, &mut upstream_indexes);
            self.initialise_upstreams(&upstreams.passive, false, &mut layer, &mut upstream_indexes);
            let node_data = NodeData {
                node: node.clone(),
                upstreams: upstream_indexes,
                downstreams: vec![],
                layer,
                active: true,
            };
            let index = self.state.nodes.len();
            self.state.push_node(node.clone());
            self.state.nodes.push(node_data);
            index
        }
    }

    fn mark_dirty(&mut self, index: usize) {
        if !self.state.node_dirty[index] {
            let layer = self.state.nodes[index].layer;
            self.state.dirty_nodes_by_layer[layer].push(index);
            self.state.node_dirty[index] = true;
        }
    }

    fn process_scheduled_callbacks(&mut self) -> bool {
        let mut progressed = false;
        for i in 0..self.state.always_callbacks.len() {
            let ix = self.state.always_callbacks[i];
            self.mark_dirty(ix);
            progressed = true;
        }
        while self.state.has_pending_scheduled_callbacks() {
            let Some(ix) = self.state.scheduled_callbacks.pop() else {
                break;
            };
            self.mark_dirty(ix);
            progressed = true;
        }
        progressed
    }

    fn process_callbacks_historical(&mut self) -> bool {
        if !self.state.ready_callbacks.is_empty() {
            panic!("ready_callbacks are not supported in realtime mode.");
        }
        if self.state.has_scheduled_callbacks() {
            let next = self.state.next_scheduled_time();
            self.state.time = if self.state.first_cycle {
                // First cycle: use the scheduled time as-is (may be NanoTime::ZERO).
                self.state.first_cycle = false;
                next
            } else {
                // Enforce strict monotonic progression: bump to prev+1 if needed.
                next.max(self.state.time + 1)
            };
        }
        self.process_scheduled_callbacks()
    }

    fn process_ready_callbacks(&mut self) -> bool {
        let mut progressed = false;
        while let Ok(ix) = self.state.ready_callbacks.try_recv() {
            self.mark_dirty(ix);
            progressed = true;
        }
        progressed
    }

    fn process_callbacks_realtime(&mut self, end_time: NanoTime) -> bool {
        let mut progressed = self.process_ready_callbacks();
        if self.process_scheduled_callbacks() {
            progressed = true;
        }
        if !progressed {
            let wait_until = min(end_time, self.state.next_scheduled_time());
            if let Some(ix) = self.state.wait_ready_callback(wait_until) {
                self.mark_dirty(ix);
                progressed = true;
            }
        }
        self.state.time = NanoTime::now().max(self.state.time + 1);
        progressed
    }

    #[cfg_attr(feature = "instrument-cycle", tracing::instrument(skip_all))]
    fn cycle(&mut self) -> anyhow::Result<()> {
        // Snap wall-clock time once per cycle for latency / perf telemetry.
        // Separate from `state.time` so historical mode still has deterministic
        // logical time for business logic.
        self.state.wall_time = NanoTime::now();
        for lyr in 0..self.state.dirty_nodes_by_layer.len() {
            for i in 0..self.state.dirty_nodes_by_layer[lyr].len() {
                let ix = self.state.dirty_nodes_by_layer[lyr][i];
                self.cycle_node(ix)?;
            }
        }
        self.reset();
        #[cfg(feature = "dynamic-graph-beta")]
        self.process_pending_removals()?;
        #[cfg(feature = "dynamic-graph-beta")]
        self.process_pending_additions()?;
        Ok(())
    }

    #[cfg_attr(
        feature = "instrument-cycle-node",
        tracing::instrument(skip(self), fields(node = tracing::field::Empty))
    )]
    fn cycle_node(&mut self, index: usize) -> anyhow::Result<()> {
        if !self.state.nodes[index].active {
            return Ok(());
        }
        #[cfg(feature = "instrument-cycle-node")]
        tracing::Span::current().record("node", self.state.nodes[index].node.type_name());
        let node = &self.state.nodes[index].node;
        self.state.current_node_index = Some(index);
        let result = node.clone().cycle(&mut self.state);
        self.state.current_node_index = None;

        let ticked = result.map_err(|e| {
            let context = self.format_context(index, 3);
            e.context(format!("Error in node [{index}]:\n{context}"))
        })?;

        if ticked {
            self.state.set_ticked(index);
            for i in 0..self.state.nodes[index].downstreams.len() {
                let (downstream_index, active) = self.state.nodes[index].downstreams[i];
                if active {
                    self.mark_dirty(downstream_index)
                }
            }
        }
        Ok(())
    }

    fn reset(&mut self) {
        self.state.reset();
        for layer in self.state.dirty_nodes_by_layer.iter_mut() {
            layer.clear();
        }
        // compiler actually optimises this into memset :)
        // https://users.rust-lang.org/t/fastest-way-to-zero-an-array/39222
        for i in self.state.node_dirty.iter_mut() {
            *i = false;
        }
    }

    #[cfg(feature = "dynamic-graph-beta")]
    fn process_pending_removals(&mut self) -> anyhow::Result<()> {
        let removals = std::mem::take(&mut self.state.pending_removals);
        for node in removals {
            let Some(index) = self.state.node_index(node.clone()) else {
                continue;
            };
            if !self.state.nodes[index].active {
                continue;
            }
            // Unlink from upstreams' downstreams
            let upstreams: Vec<(usize, bool)> = self.state.nodes[index].upstreams.clone();
            for (up_idx, _) in &upstreams {
                self.state.nodes[*up_idx]
                    .downstreams
                    .retain(|(di, _)| *di != index);
            }
            // Unlink from downstreams' upstreams
            let downstreams: Vec<(usize, bool)> = self.state.nodes[index].downstreams.clone();
            for (dn_idx, _) in &downstreams {
                self.state.nodes[*dn_idx]
                    .upstreams
                    .retain(|(ui, _)| *ui != index);
            }
            // stop + teardown
            self.state.current_node_index = Some(index);
            node.clone().stop(&mut self.state).map_err(|e| {
                e.context(format!(
                    "Error during stop in node [{index}] (pending removal)"
                ))
            })?;
            node.clone().teardown(&mut self.state).map_err(|e| {
                e.context(format!(
                    "Error during teardown in node [{index}] (pending removal)"
                ))
            })?;
            self.state.current_node_index = None;
            self.state.nodes[index].active = false;
        }
        Ok(())
    }

    #[cfg(feature = "dynamic-graph-beta")]
    fn process_pending_additions(&mut self) -> anyhow::Result<()> {
        let additions = std::mem::take(&mut self.state.pending_additions);
        if additions.is_empty() {
            return Ok(());
        }
        let start_index = self.state.nodes.len();

        // Recurse through each new subgraph; seen() prevents re-registering existing nodes.
        for (node, _caller_index, _is_active, _recycle) in &additions {
            if !self.state.seen(node.clone()) {
                self.initialise_node(node);
            }
        }

        // Indices of truly new nodes
        let new_indices: Vec<usize> = (start_index..self.state.nodes.len()).collect();

        // Push node_dirty entries for new nodes (node_ticked already pushed by push_node)
        for _ in &new_indices {
            self.state.node_dirty.push(false);
        }

        // Wire declared upstreams' downstreams for new nodes
        for &ix in &new_indices {
            let upstreams = self.state.nodes[ix].upstreams.clone();
            for (up_idx, active) in upstreams {
                self.state.nodes[up_idx].downstreams.push((ix, active));
            }
        }

        // Wire the dynamic caller→node edges and fix layers.
        // Track wired (caller, node) pairs to skip duplicates if add_upstream
        // is called more than once with the same node in a single cycle.
        let mut wired_edges: HashSet<(usize, usize)> = HashSet::new();
        for (node, caller_index, is_active, recycle) in &additions {
            let node_index = self.state.node_index(node.clone()).ok_or_else(|| {
                anyhow::anyhow!("node not found in graph after dynamic initialization")
            })?;
            if wired_edges.insert((*caller_index, node_index)) {
                self.state.nodes[*caller_index]
                    .upstreams
                    .push((node_index, *is_active));
                self.state.nodes[node_index]
                    .downstreams
                    .push((*caller_index, *is_active));
            }
            self.fix_layers(*caller_index);
            if *recycle {
                let time = self.state.time + NanoTime::new(1);
                let new_set: HashSet<usize> = new_indices.iter().cloned().collect();
                // Walk upward from the leaf through the newly-added subgraph to find
                // attachment points: new nodes that connect directly to the pre-existing
                // graph (have at least one pre-existing upstream) or are new sources
                // (have no upstreams at all).  Scheduling recycle callbacks there runs
                // the pipeline in correct dependency order instead of firing the leaf
                // with stale default values from intermediate nodes.
                let mut stack = vec![node_index];
                let mut visited: HashSet<usize> = HashSet::new();
                while let Some(ix) = stack.pop() {
                    if !visited.insert(ix) {
                        continue;
                    }
                    let upstreams: Vec<(usize, bool)> = self.state.nodes[ix].upstreams.clone();
                    let has_preexisting = upstreams.iter().any(|(up, _)| !new_set.contains(up));
                    let is_source = upstreams.is_empty();
                    if has_preexisting || is_source {
                        self.state.add_callback_for_node(ix, time);
                    }
                    for &(up_idx, _) in &upstreams {
                        if new_set.contains(&up_idx) {
                            stack.push(up_idx);
                        }
                    }
                }
            }
        }

        // Batch setup then batch start for new nodes only
        for &ix in &new_indices {
            let node = self.state.nodes[ix].node.clone();
            self.state.current_node_index = Some(ix);
            node.setup(&mut self.state).map_err(|e| {
                e.context(format!(
                    "Error during setup in node [{ix}] (dynamic addition)"
                ))
            })?;
            self.state.current_node_index = None;
        }
        for &ix in &new_indices {
            let node = self.state.nodes[ix].node.clone();
            self.state.current_node_index = Some(ix);
            node.start(&mut self.state).map_err(|e| {
                e.context(format!(
                    "Error during start in node [{ix}] (dynamic addition)"
                ))
            })?;
            self.state.current_node_index = None;
        }
        Ok(())
    }

    /// Recalculate `node_index`'s layer based on its current upstreams,
    /// propagate any increase to its downstreams, and extend
    /// `dirty_nodes_by_layer` to accommodate the new layer.
    ///
    /// Iterative BFS to avoid stack overflows on deep graphs.
    #[cfg(feature = "dynamic-graph-beta")]
    fn fix_layers(&mut self, start: usize) {
        let mut queue = std::collections::VecDeque::new();
        queue.push_back(start);
        while let Some(node_index) = queue.pop_front() {
            let required = self.state.nodes[node_index]
                .upstreams
                .iter()
                .map(|(up_idx, _)| self.state.nodes[*up_idx].layer)
                .max()
                .map_or(0, |m| m + 1);
            if required > self.state.nodes[node_index].layer {
                self.state.nodes[node_index].layer = required;
                let downstreams: Vec<usize> = self.state.nodes[node_index]
                    .downstreams
                    .iter()
                    .map(|(di, _)| *di)
                    .collect();
                for dn_idx in downstreams {
                    queue.push_back(dn_idx);
                }
            }
            let layer = self.state.nodes[node_index].layer;
            while self.state.dirty_nodes_by_layer.len() <= layer {
                self.state.dirty_nodes_by_layer.push(vec![]);
            }
        }
    }

    /// Format nodes around the given index for error context.
    /// Shows `range` nodes before and after, marking the target node.
    fn format_context(&self, target_index: usize, range: usize) -> String {
        let mut output = String::new();
        let start = target_index.saturating_sub(range);
        let end = (target_index + range + 1).min(self.state.nodes.len());

        for i in start..end {
            let node_data = &self.state.nodes[i];
            let marker = if i == target_index { ">>> " } else { "    " };
            output.push_str(&format!("{marker}[{i:02}] "));
            for _ in 0..node_data.layer {
                output.push_str("   ");
            }
            output.push_str(&format!("{}\n", node_data.node));
        }
        output
    }

    pub fn print(&mut self) -> &mut Graph {
        for (i, node_data) in self.state.nodes.iter().enumerate() {
            print!("[{i:02}] ");
            for _ in 0..node_data.layer {
                print!("   ");
            }
            println!("{:}", node_data.node);
        }
        self
    }

    pub fn export(&self, path: &str) -> Result<(), Error> {
        let path = Path::new(&path);
        let mut output = File::create(path)?;
        writeln!(output, "graph [")?;
        for i in 0..self.state.nodes.len() {
            writeln!(output, "    node [")?;
            writeln!(output, "        id {i}")?;
            writeln!(
                output,
                "        label \"[{i}] {}\"",
                self.state.nodes[i].node
            )?;
            writeln!(output, "        graphics")?;
            writeln!(output, "        [")?;
            writeln!(output, "            w 200.0")?;
            writeln!(output, "            h 30.0")?;
            writeln!(output, "        ]")?;
            writeln!(output, "    ]")?;
        }
        for (i, node) in self.state.nodes.iter().enumerate() {
            for downstream in node.downstreams.iter() {
                let downstream_index = downstream.0;
                writeln!(output, "    edge [")?;
                writeln!(output, "        source {i}")?;
                writeln!(output, "        target {downstream_index}")?;
                writeln!(output, "    ]")?;
            }
        }
        writeln!(output, "]")
    }
}

#[cfg(test)]
mod tests {

    use crate::graph::*;
    use crate::nodes::*;
    use crate::queue::ValueAt;
    use crate::types::*;
    use std::cell::RefCell;

    use itertools::Itertools;

    // ── RunFor::done ─────────────────────────────────────────────────────────

    #[test]
    fn run_for_cycles_done_when_exceeded() {
        let rf = RunFor::Cycles(3);
        assert!(!rf.done(3, NanoTime::ZERO)); // cycle == limit → not done
        assert!(rf.done(4, NanoTime::ZERO)); // cycle > limit → done
    }

    #[test]
    fn run_for_duration_done_when_elapsed_exceeds() {
        use std::time::Duration;
        let rf = RunFor::Duration(Duration::from_nanos(100));
        assert!(!rf.done(0, NanoTime::new(100))); // equal → not done
        assert!(rf.done(0, NanoTime::new(101))); // exceeded → done
    }

    #[test]
    fn run_for_forever_never_done() {
        let rf = RunFor::Forever;
        assert!(!rf.done(u32::MAX, NanoTime::MAX));
    }

    // ── average_duration ─────────────────────────────────────────────────────

    #[test]
    fn average_duration_normal() {
        // function is private — exercise it indirectly via the graph cycle timing
        // by running a trivial graph and checking it doesn't panic.
        let src = Rc::new(RefCell::new(CallBackStream::<u64>::new()));
        src.borrow_mut().push(ValueAt::new(1u64, NanoTime::new(10)));
        src.clone()
            .as_stream()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();
        // If average_duration panicked we wouldn't reach here.
        // Also cover the n=0 branch by running a graph that completes normally.
        let result = Graph::new(
            vec![Rc::new(RefCell::new(CallBackStream::<u64>::new())).as_node()],
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(1),
        )
        .run();
        assert!(result.is_ok());
    }

    // ── average_duration (private fn) ────────────────────────────────────────

    #[test]
    fn average_duration_zero_n_returns_zero() {
        use std::time::Duration;
        // n=0 branch
        assert_eq!(average_duration(Duration::from_secs(10), 0), Duration::ZERO);
    }

    #[test]
    fn average_duration_normal_case() {
        use std::time::Duration;
        // 100ns / 4 = 25ns
        assert_eq!(
            average_duration(Duration::from_nanos(100), 4),
            Duration::from_nanos(25)
        );
    }

    // ── GraphState::node_index_ticked (pub(crate)) ────────────────────────────

    #[test]
    fn node_index_ticked_reflects_cycle() {
        let src = Rc::new(RefCell::new(CallBackStream::<u64>::new()));
        src.borrow_mut().push(ValueAt::new(1u64, NanoTime::new(1)));
        let cnt = src.clone().as_stream().count();
        cnt.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        // Just verify the fn exists and is callable by using it indirectly.
        // GraphState is not directly accessible after run(), but the fn is
        // exercised internally. We test the function directly:
        let mut state = GraphState::new(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(1),
            NanoTime::ZERO,
        );
        state.node_ticked.push(false);
        assert!(!state.node_index_ticked(0));
        state.node_ticked[0] = true;
        assert!(state.node_index_ticked(0));
    }

    // ── GraphState::log (when current_node_index is None) ────────────────────

    #[test]
    fn graph_state_log_with_no_current_node_is_noop() {
        let state = GraphState::new(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(1),
            NanoTime::ZERO,
        );
        // current_node_index is None → should return immediately without panic
        state.log(log::Level::Info, "test message");
    }

    // ── Graph::export ─────────────────────────────────────────────────────────

    #[test]
    fn graph_export_writes_gml_file() {
        use std::fs;
        let src: Rc<dyn Stream<u64>> = Rc::new(RefCell::new(CallBackStream::<u64>::new()));
        let mapped = src.map(|v| v + 1);
        let graph = mapped.into_graph(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(0));
        let path = "/tmp/wingfoil_test_export.gml";
        graph.export(path).unwrap();
        let content = fs::read_to_string(path).unwrap();
        assert!(content.contains("graph ["));
        assert!(content.contains("node ["));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn historical_mode_works() {
        // wire up graph..
        //env_logger::init();
        let num_inputs = 7;
        let inputs: Vec<Rc<RefCell<CallBackStream<i32>>>> = (0..num_inputs)
            .map(|_| Rc::new(RefCell::new(CallBackStream::new())))
            .collect();
        let captured = inputs
            .iter()
            .map(|stream| stream.clone().as_stream().distinct())
            .tree_reduce(
                // https://docs.rs/itertools/0.8.0/itertools/trait.Itertools.html#method.tree_fold1
                // 1 2 3 4 5 6 7
                // │ │ │ │ │ │ │
                // └─f └─f └─f │
                //   │   │   │ │
                //   └───f   └─f
                //       │     │
                //       └─────f
                |a, b| add(&a, &b),
            )
            .unwrap()
            .collect();
        let mut expected: Vec<ValueAt<i32>> = vec![];
        // mock up input data
        // at future time 100 push all inputs with value 1
        push_all(
            &inputs,
            ValueAt {
                value: 1,
                time: NanoTime::new(100),
            },
        );
        expected.push(ValueAt {
            value: 7,
            time: NanoTime::new(100),
        });
        // at future time 200 push all inputs with value 1 again.
        push_all(
            &inputs,
            ValueAt {
                value: 1,
                time: NanoTime::new(200),
            },
        );
        // nothing expected - only input layer ticked because it is wrapped in distinct.
        // at future time 300 push first input with value 2
        push_first(
            &inputs,
            ValueAt {
                value: 2,
                time: NanoTime::new(300),
            },
        );
        expected.push(ValueAt {
            value: 8,
            time: NanoTime::new(300),
        });
        // at future time 400 push first input with value 2 again.  Only first input node ticks.
        push_first(
            &inputs,
            ValueAt {
                value: 2,
                time: NanoTime::new(400),
            },
        );
        // nothing expected
        // wire up graph and set historical mode
        let run_mode = RunMode::HistoricalFrom(NanoTime::ZERO);
        Graph::new(vec![captured.clone().as_node()], run_mode, RunFor::Forever)
            .print()
            .run()
            .unwrap();
        //.export("historical_mode_works")
        //.unwrap();
        let captured_data = captured.peek_value();
        println!();
        println!("captured_data  {captured_data:?}");
        println!("expected       {expected:?}");
        println!();
        assert_eq!(captured_data, expected);
    }

    #[test]
    fn error_context_shows_graph_structure() {
        use std::time::Duration;

        // Build deep graph: ticker -> count -> 10 maps -> try_map (fails) -> 10 maps
        let mut stream: Rc<dyn Stream<u64>> = ticker(Duration::from_nanos(100)).count();

        // 10 maps before try_map
        for _ in 0..10 {
            stream = stream.map(|x: u64| x);
        }

        // try_map that fails on count == 3
        stream = stream.try_map(|x: u64| {
            if x == 3 {
                anyhow::bail!("intentional failure at count 3")
            } else {
                Ok(x)
            }
        });

        // 10 maps after try_map
        for _ in 0..10 {
            stream = stream.map(|x: u64| x);
        }

        let mut graph = Graph::new(
            vec![stream.as_node()],
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(10),
        );
        graph.print();
        let result = graph.run();

        assert!(result.is_err(), "Expected error but got: {result:?}");
        let err_msg = format!("{:?}", result.unwrap_err());

        let expected = r#"Error in node [14]:
    [11]                               MapStream<u64, u64>
    [12]                                  MapStream<u64, u64>
    [13]                                     MapStream<u64, u64>
>>> [14]                                        TryMapStream<u64, u64>
    [15]                                           MapStream<u64, u64>
    [16]                                              MapStream<u64, u64>
    [17]                                                 MapStream<u64, u64>


Caused by:
    intentional failure at count 3"#;

        assert!(
            err_msg.contains(expected),
            "Error message mismatch.\n\nExpected to contain:\n{expected}\n\nActual:\n{err_msg}"
        );
    }

    fn push_all(inputs: &[Rc<RefCell<CallBackStream<i32>>>], value_at: ValueAt<i32>) {
        inputs
            .iter()
            .for_each(|input| input.borrow_mut().push(value_at.clone()));
    }

    fn push_first(inputs: &[Rc<RefCell<CallBackStream<i32>>>], value_at: ValueAt<i32>) {
        inputs[0].borrow_mut().push(value_at);
    }

    /// Minimal test node: records state.time() on each cycle, and on its first
    /// cycle re-schedules itself at a caller-supplied time (which may equal or
    /// precede the current engine time).
    struct TimeCapturingNode {
        times: Vec<NanoTime>,
        resched_time: NanoTime,
    }

    impl MutableNode for TimeCapturingNode {
        fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
            let t = state.time();
            self.times.push(t);
            if self.times.len() == 1 {
                state.add_callback(self.resched_time)?;
            }
            Ok(true)
        }

        fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
            state.add_callback(NanoTime::new(100))?;
            Ok(())
        }
    }

    /// Time must advance even when a node re-schedules itself at the current time.
    #[test]
    fn time_advances_with_duplicate_scheduled_time() {
        let node = Rc::new(RefCell::new(TimeCapturingNode {
            times: vec![],
            resched_time: NanoTime::new(100), // same as initial fire time
        }));
        Graph::new(
            vec![node.clone().as_node()],
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(2),
        )
        .run()
        .unwrap();

        let times = node.borrow().times.clone();
        assert_eq!(times.len(), 2, "expected exactly 2 cycles");
        assert!(
            times[1] > times[0],
            "time did not advance between cycles: {:?} -> {:?}",
            times[0],
            times[1]
        );
    }

    /// Time must not go backwards when a node re-schedules itself in the past.
    #[test]
    fn time_advances_with_past_scheduled_time() {
        let node = Rc::new(RefCell::new(TimeCapturingNode {
            times: vec![],
            resched_time: NanoTime::new(50), // in the past relative to initial fire at 100
        }));
        Graph::new(
            vec![node.clone().as_node()],
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(2),
        )
        .run()
        .unwrap();

        let times = node.borrow().times.clone();
        assert_eq!(times.len(), 2, "expected exactly 2 cycles");
        assert!(
            times[1] > times[0],
            "time went backwards or stalled: {:?} -> {:?}",
            times[0],
            times[1]
        );
    }

    // ── Dynamism tests ────────────────────────────────────────────────────────

    #[test]
    fn ticked_on_unregistered_node_returns_false() {
        use std::time::Duration;
        let orphan = ticker(Duration::from_nanos(1)).count();
        let state = GraphState::new(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(1),
            NanoTime::ZERO,
        );
        // Must return false (not panic) for a node not yet registered in the graph
        assert!(!state.ticked(orphan.as_node()));
    }

    #[cfg(feature = "dynamic-graph-beta")]
    mod dynamism {
        use super::*;

        /// A node that, after `add_after` cycles, calls `state.add_upstream(extra, true, false)`.
        /// Counts how many times `extra` has ticked via `extra_ticks`.
        struct DynAdderNode {
            trigger: Rc<dyn Node>,
            extra: Rc<dyn Node>,
            add_after: u64,
            cycle_count: u64,
            extra_ticks: Rc<RefCell<u64>>,
            stop_count: Rc<RefCell<u64>>,
            teardown_count: Rc<RefCell<u64>>,
        }

        impl MutableNode for DynAdderNode {
            fn upstreams(&self) -> UpStreams {
                UpStreams::new(vec![self.trigger.clone()], vec![])
            }

            fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
                self.cycle_count += 1;
                if self.cycle_count == self.add_after {
                    state.add_upstream(self.extra.clone(), true, false)?;
                }
                if state.ticked(self.extra.clone()) {
                    *self.extra_ticks.borrow_mut() += 1;
                }
                Ok(true)
            }

            fn stop(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
                *self.stop_count.borrow_mut() += 1;
                Ok(())
            }

            fn teardown(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
                *self.teardown_count.borrow_mut() += 1;
                Ok(())
            }
        }

        /// A stream that counts how many times its trigger has ticked.
        /// Unlike `count()`, it does not use `constant` internally, so it never
        /// schedules a past-time callback and is safe to add dynamically.
        struct SimpleCounter {
            trigger: Rc<dyn Node>,
            n: u64,
        }

        impl MutableNode for SimpleCounter {
            fn upstreams(&self) -> UpStreams {
                UpStreams::new(vec![self.trigger.clone()], vec![])
            }

            fn cycle(&mut self, _: &mut GraphState) -> anyhow::Result<bool> {
                self.n += 1;
                Ok(true)
            }
        }

        impl StreamPeekRef<u64> for SimpleCounter {
            fn peek_ref(&self) -> &u64 {
                &self.n
            }
        }

        /// A node that records setup/start/cycle/stop/teardown call counts.
        struct LifecycleCounterNode {
            trigger: Rc<dyn Node>,
            cycle_count: Rc<RefCell<u64>>,
            stop_count: Rc<RefCell<u64>>,
            teardown_count: Rc<RefCell<u64>>,
        }

        impl MutableNode for LifecycleCounterNode {
            fn upstreams(&self) -> UpStreams {
                UpStreams::new(vec![self.trigger.clone()], vec![])
            }

            fn cycle(&mut self, _: &mut GraphState) -> anyhow::Result<bool> {
                *self.cycle_count.borrow_mut() += 1;
                Ok(true)
            }

            fn stop(&mut self, _: &mut GraphState) -> anyhow::Result<()> {
                *self.stop_count.borrow_mut() += 1;
                Ok(())
            }

            fn teardown(&mut self, _: &mut GraphState) -> anyhow::Result<()> {
                *self.teardown_count.borrow_mut() += 1;
                Ok(())
            }
        }

        #[test]
        fn add_upstream_dynamically_fires_only_after_wired() {
            use std::time::Duration;

            let ticker = ticker(Duration::from_nanos(1));
            // Use SimpleCounter instead of ticker.count() to avoid the constant
            // node scheduling a past-time callback when added dynamically.
            let extra: Rc<dyn Stream<u64>> = SimpleCounter {
                trigger: ticker.clone(),
                n: 0,
            }
            .into_stream();

            let extra_ticks = Rc::new(RefCell::new(0u64));
            let stop_count = Rc::new(RefCell::new(0u64));
            let teardown_count = Rc::new(RefCell::new(0u64));

            // adder is triggered by ticker; after cycle 3 it adds `extra` as an upstream
            let adder = Rc::new(RefCell::new(DynAdderNode {
                trigger: ticker.clone(),
                extra: extra.clone().as_node(),
                add_after: 3,
                cycle_count: 0,
                extra_ticks: extra_ticks.clone(),
                stop_count: stop_count.clone(),
                teardown_count: teardown_count.clone(),
            }));

            Graph::new(
                vec![adder.clone().as_node()],
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Cycles(6),
            )
            .run()
            .unwrap();

            // extra was wired at the end of cycle 3; it should tick on cycles 4, 5, 6 → 3 times
            assert_eq!(*extra_ticks.borrow(), 3, "extra_ticks");
        }

        /// A node that removes `target` from the graph after `remove_after` ticks.
        struct DynRemoverNode {
            trigger: Rc<dyn Node>,
            target: Rc<dyn Node>,
            remove_after: u64,
            cycle_count: u64,
        }

        impl MutableNode for DynRemoverNode {
            fn upstreams(&self) -> UpStreams {
                UpStreams::new(vec![self.trigger.clone()], vec![])
            }

            fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
                self.cycle_count += 1;
                if self.cycle_count == self.remove_after {
                    state.remove_node(self.target.clone());
                }
                Ok(true)
            }
        }

        #[test]
        fn remove_node_stops_firing_and_calls_lifecycle() {
            use std::time::Duration;

            let ticker = ticker(Duration::from_nanos(1));
            let extra_ticks = Rc::new(RefCell::new(0u64));
            let stop_count = Rc::new(RefCell::new(0u64));
            let teardown_count = Rc::new(RefCell::new(0u64));

            let extra_ticks_c = extra_ticks.clone();
            let stop_c = stop_count.clone();
            let teardown_c = teardown_count.clone();

            // adder: after cycle 1, wires `extra` (count stream) as an upstream
            let extra = ticker.count();
            let adder = Rc::new(RefCell::new(DynAdderNode {
                trigger: ticker.clone(),
                extra: extra.clone().as_node(),
                add_after: 1,
                cycle_count: 0,
                extra_ticks: extra_ticks_c,
                stop_count: stop_c,
                teardown_count: teardown_c,
            }));

            // remover: after cycle 5, removes adder from the graph
            let remover = Rc::new(RefCell::new(DynRemoverNode {
                trigger: ticker.clone(),
                target: adder.clone().as_node(),
                remove_after: 5,
                cycle_count: 0,
            }));

            Graph::new(
                vec![adder.clone().as_node(), remover.clone().as_node()],
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Cycles(8),
            )
            .run()
            .unwrap();

            // adder fires on cycles 1..=8 = 8 times; extra wired at end of cycle 1,
            // so extra fires on cycles 2..=8 → but adder is removed at end of cycle 5,
            // so extra_ticks should be counted on cycles 2,3,4,5 = 4
            assert_eq!(*extra_ticks.borrow(), 4, "extra_ticks after removal");
            // stop + teardown each called exactly once (at removal, not at graph shutdown)
            assert_eq!(*stop_count.borrow(), 1, "stop_count");
            assert_eq!(*teardown_count.borrow(), 1, "teardown_count");
        }

        #[test]
        fn remove_cleans_up_caller_upstreams() {
            use std::time::Duration;

            let ticker = ticker(Duration::from_nanos(1));
            let extra = ticker.count();
            let extra_ticks = Rc::new(RefCell::new(0u64));
            let stop_count = Rc::new(RefCell::new(0u64));
            let teardown_count = Rc::new(RefCell::new(0u64));

            // Wire extra at cycle 1, then remove it at cycle 3
            let adder = Rc::new(RefCell::new(DynAdderNode {
                trigger: ticker.clone(),
                extra: extra.clone().as_node(),
                add_after: 1,
                cycle_count: 0,
                extra_ticks: extra_ticks.clone(),
                stop_count: stop_count.clone(),
                teardown_count: teardown_count.clone(),
            }));

            let remover = Rc::new(RefCell::new(DynRemoverNode {
                trigger: ticker.clone(),
                target: extra.clone().as_node(),
                remove_after: 3,
                cycle_count: 0,
            }));

            let mut graph = Graph::new(
                vec![adder.clone().as_node(), remover.clone().as_node()],
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Cycles(6),
            );
            graph.run().unwrap();

            // After removal, adder's upstreams should not contain the removed node's index
            let adder_idx = graph
                .state
                .node_index(adder.clone().as_node())
                .expect("adder in graph");
            let extra_idx = graph.state.node_index(extra.clone().as_node());
            if let Some(extra_idx) = extra_idx {
                let adder_upstreams = &graph.state.nodes[adder_idx].upstreams;
                assert!(
                    !adder_upstreams.iter().any(|(ui, _)| *ui == extra_idx),
                    "removed node should not remain in caller's upstreams"
                );
            }
        }

        #[test]
        fn add_upstream_with_recycle_delivers_first_value() {
            use std::time::Duration;

            // A node that adds `extra` as an upstream on cycle 1 with recycle=true,
            // and then records extra's value every time it ticks.
            struct RecycleNode {
                trigger: Rc<dyn Node>,
                extra: Rc<dyn Stream<u64>>,
                added: bool,
                values: Rc<RefCell<Vec<u64>>>,
            }

            impl MutableNode for RecycleNode {
                fn upstreams(&self) -> UpStreams {
                    UpStreams::new(vec![self.trigger.clone()], vec![])
                }

                fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
                    if !self.added {
                        state.add_upstream(self.extra.clone().as_node(), true, true)?;
                        self.added = true;
                    }
                    if state.ticked(self.extra.clone().as_node()) {
                        self.values.borrow_mut().push(self.extra.peek_value());
                    }
                    Ok(true)
                }
            }

            let ticker = ticker(Duration::from_nanos(1));
            // Use SimpleCounter to avoid constant's past-time start callback.
            let extra: Rc<dyn Stream<u64>> = SimpleCounter {
                trigger: ticker.clone(),
                n: 0,
            }
            .into_stream();
            let values = Rc::new(RefCell::new(Vec::<u64>::new()));

            let node = Rc::new(RefCell::new(RecycleNode {
                trigger: ticker.clone(),
                extra: extra.clone(),
                added: false,
                values: values.clone(),
            }));

            Graph::new(
                vec![node.clone().as_node()],
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Cycles(3),
            )
            .run()
            .unwrap();

            // Cycle 1 (t=0ns): ticker fires. RecycleNode wires extra with recycle=true.
            // process_pending_additions schedules the recycle callback at state.time+1 = 1ns.
            // Cycle 2 (t=1ns): ticker fires (2nd tick) AND recycle fires extra — both at 1ns.
            //   extra cycles → n=1. RecycleNode triggered → values=[1].
            // Cycle 3 (t=2ns): ticker fires (3rd tick). extra cycles → n=2. values=[1,2].
            // RunFor::Cycles(3) → 3 cycles total → values = [1, 2].
            assert_eq!(*values.borrow(), vec![1u64, 2], "recycle values");
        }

        #[test]
        fn add_upstream_passive_does_not_trigger() {
            use std::time::Duration;

            // A node that adds `extra` as a PASSIVE upstream; should not be triggered by it.
            struct PassiveNode {
                trigger: Rc<dyn Node>,
                extra: Rc<dyn Stream<u64>>,
                added: bool,
                trigger_ticks: u64,
                extra_observed: Rc<RefCell<Vec<u64>>>,
            }

            impl MutableNode for PassiveNode {
                fn upstreams(&self) -> UpStreams {
                    UpStreams::new(vec![self.trigger.clone()], vec![])
                }

                fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
                    if !self.added {
                        // passive upstream: is_active = false
                        state.add_upstream(self.extra.clone().as_node(), false, false)?;
                        self.added = true;
                    }
                    // We are only triggered by `trigger`, not by `extra`.
                    // But we can still peek `extra`'s value.
                    if state.ticked(self.trigger.clone()) {
                        self.trigger_ticks += 1;
                        self.extra_observed
                            .borrow_mut()
                            .push(self.extra.peek_value());
                    }
                    Ok(true)
                }
            }

            let ticker = ticker(Duration::from_nanos(1));
            // Use SimpleCounter to avoid constant's past-time start callback.
            let extra: Rc<dyn Stream<u64>> = SimpleCounter {
                trigger: ticker.clone(),
                n: 0,
            }
            .into_stream();
            let extra_observed = Rc::new(RefCell::new(Vec::<u64>::new()));

            let node = Rc::new(RefCell::new(PassiveNode {
                trigger: ticker.clone(),
                extra: extra.clone(),
                added: false,
                trigger_ticks: 0,
                extra_observed: extra_observed.clone(),
            }));

            Graph::new(
                vec![node.clone().as_node()],
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Cycles(4),
            )
            .run()
            .unwrap();

            // The node ticked 4 times (triggered by ticker each cycle).
            // extra is a passive upstream — node is NOT triggered by extra.
            assert_eq!(extra_observed.borrow().len(), 4);
        }

        #[test]
        fn seen_prevents_double_setup_start() {
            use std::time::Duration;

            struct CountedNode {
                ticker: Rc<dyn Node>,
                setup_count: Rc<RefCell<u32>>,
                start_count: Rc<RefCell<u32>>,
            }

            impl MutableNode for CountedNode {
                fn upstreams(&self) -> UpStreams {
                    UpStreams::new(vec![self.ticker.clone()], vec![])
                }

                fn cycle(&mut self, _state: &mut GraphState) -> anyhow::Result<bool> {
                    Ok(true)
                }

                fn setup(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
                    *self.setup_count.borrow_mut() += 1;
                    Ok(())
                }

                fn start(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
                    *self.start_count.borrow_mut() += 1;
                    Ok(())
                }
            }

            // A node that adds `counted` as upstream twice on consecutive cycles
            struct DoublerNode {
                trigger: Rc<dyn Node>,
                counted: Rc<dyn Node>,
                add_count: u64,
            }

            impl MutableNode for DoublerNode {
                fn upstreams(&self) -> UpStreams {
                    UpStreams::new(vec![self.trigger.clone()], vec![])
                }

                fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
                    if self.add_count < 2 {
                        state.add_upstream(self.counted.clone(), true, false)?;
                        self.add_count += 1;
                    }
                    Ok(true)
                }
            }

            let setup_count = Rc::new(RefCell::new(0u32));
            let start_count = Rc::new(RefCell::new(0u32));
            let ticker = ticker(Duration::from_nanos(1));
            let counted = Rc::new(RefCell::new(CountedNode {
                ticker: ticker.clone(),
                setup_count: setup_count.clone(),
                start_count: start_count.clone(),
            }));
            let doubler = Rc::new(RefCell::new(DoublerNode {
                trigger: ticker.clone(),
                counted: counted.clone().as_node(),
                add_count: 0,
            }));

            Graph::new(
                vec![doubler.clone().as_node()],
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Cycles(3),
            )
            .run()
            .unwrap();

            // setup and start should only be called once even though add_upstream was called twice
            assert_eq!(*setup_count.borrow(), 1, "setup called once");
            assert_eq!(*start_count.borrow(), 1, "start called once");
        }

        #[test]
        fn layer_resort_after_deep_upstream_addition() {
            use std::time::Duration;

            // Build: ticker → depth1 → depth2 → aggregation
            // aggregation's initial layer is > depth2's layer.
            // Then add depth2 as an upstream of aggregation — fix_layers should
            // ensure aggregation's layer is >= depth2.layer + 1.
            let ticker = ticker(Duration::from_nanos(1));
            let depth1 = ticker.count(); // layer 1
            let depth2 = depth1.map(|x: u64| x * 2); // layer 2

            // aggregation starts triggered by ticker (layer 0), so its layer = 1.
            // We then add depth2 (layer 2) as an upstream → aggregation should move to layer 3.
            struct LayerCheckNode {
                trigger: Rc<dyn Node>,
                deep: Rc<dyn Stream<u64>>,
                added: bool,
                sum: u64,
            }

            impl MutableNode for LayerCheckNode {
                fn upstreams(&self) -> UpStreams {
                    UpStreams::new(vec![self.trigger.clone()], vec![])
                }

                fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
                    if !self.added {
                        state.add_upstream(self.deep.clone().as_node(), true, false)?;
                        self.added = true;
                    }
                    if state.ticked(self.deep.clone().as_node()) {
                        self.sum += self.deep.peek_value();
                    }
                    Ok(true)
                }
            }

            let node = Rc::new(RefCell::new(LayerCheckNode {
                trigger: ticker.clone(),
                deep: depth2.clone(),
                added: false,
                sum: 0,
            }));

            let mut graph = Graph::new(
                vec![node.clone().as_node()],
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Cycles(4),
            );
            graph.run().unwrap();

            // After adding depth2, node's layer should be >= depth2's layer + 1
            let node_idx = graph
                .state
                .node_index(node.clone().as_node())
                .expect("node in graph");
            let depth2_idx = graph
                .state
                .node_index(depth2.clone().as_node())
                .expect("depth2 in graph");
            assert!(
                graph.state.nodes[node_idx].layer > graph.state.nodes[depth2_idx].layer,
                "aggregation layer should be > depth2 layer after fix_layers"
            );
        }
        #[test]
        fn add_and_remove_in_same_cycle_node_is_wired() {
            use std::time::Duration;

            // Calling add_upstream and remove_node for the same node in the same cycle:
            // process_pending_removals runs before process_pending_additions, so the
            // removal is a no-op (node is not yet in the graph) and the node ends up wired.
            struct AdderRemoverNode {
                trigger: Rc<dyn Node>,
                extra: Rc<dyn Node>,
                done: bool,
                extra_ticks: Rc<RefCell<u64>>,
            }

            impl MutableNode for AdderRemoverNode {
                fn upstreams(&self) -> UpStreams {
                    UpStreams::new(vec![self.trigger.clone()], vec![])
                }

                fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
                    if !self.done {
                        state.add_upstream(self.extra.clone(), true, false)?;
                        state.remove_node(self.extra.clone());
                        self.done = true;
                    }
                    if state.ticked(self.extra.clone()) {
                        *self.extra_ticks.borrow_mut() += 1;
                    }
                    Ok(true)
                }
            }

            let ticker = ticker(Duration::from_nanos(1));
            let extra: Rc<dyn Stream<u64>> = SimpleCounter {
                trigger: ticker.clone(),
                n: 0,
            }
            .into_stream();
            let extra_ticks = Rc::new(RefCell::new(0u64));

            let node = Rc::new(RefCell::new(AdderRemoverNode {
                trigger: ticker.clone(),
                extra: extra.clone().as_node(),
                done: false,
                extra_ticks: extra_ticks.clone(),
            }));

            Graph::new(
                vec![node.clone().as_node()],
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Cycles(4),
            )
            .run()
            .unwrap();

            // Removal was a no-op (ran before addition), so extra is wired and fires
            // on cycles 2, 3, 4 → 3 ticks.
            assert_eq!(*extra_ticks.borrow(), 3, "extra_ticks");
        }

        #[test]
        fn remove_node_that_never_cycled_calls_lifecycle() {
            use std::time::Duration;

            // Wire a node whose trigger never fires (a CallBackStream with no callbacks
            // scheduled), so cycle() is never called on it. Removing it should still
            // invoke stop() and teardown() exactly once.
            //
            // Note: a ticker is unsuitable here because even a 1-hour ticker fires once
            // at t=0 in HistoricalFrom mode, which would cause extra to cycle.
            let clk = ticker(Duration::from_nanos(1));
            let never = Rc::new(RefCell::new(CallBackStream::<i32>::new()));

            let cycle_count = Rc::new(RefCell::new(0u64));
            let stop_count = Rc::new(RefCell::new(0u64));
            let teardown_count = Rc::new(RefCell::new(0u64));

            let extra = Rc::new(RefCell::new(LifecycleCounterNode {
                trigger: never.clone().as_node(),
                cycle_count: cycle_count.clone(),
                stop_count: stop_count.clone(),
                teardown_count: teardown_count.clone(),
            }));

            // Wire extra at cycle 1, remove it at cycle 3.
            let adder = Rc::new(RefCell::new(DynAdderNode {
                trigger: clk.clone(),
                extra: extra.clone().as_node(),
                add_after: 1,
                cycle_count: 0,
                extra_ticks: Rc::new(RefCell::new(0u64)),
                stop_count: Rc::new(RefCell::new(0u64)),
                teardown_count: Rc::new(RefCell::new(0u64)),
            }));

            let remover = Rc::new(RefCell::new(DynRemoverNode {
                trigger: clk.clone(),
                target: extra.clone().as_node(),
                remove_after: 3,
                cycle_count: 0,
            }));

            Graph::new(
                vec![adder.clone().as_node(), remover.clone().as_node()],
                RunMode::HistoricalFrom(NanoTime::ZERO),
                RunFor::Cycles(5),
            )
            .run()
            .unwrap();

            assert_eq!(*cycle_count.borrow(), 0, "extra cycle() never called");
            assert_eq!(*stop_count.borrow(), 1, "stop called once on removal");
            assert_eq!(
                *teardown_count.borrow(),
                1,
                "teardown called once on removal"
            );
        }
    } // mod dynamism
}

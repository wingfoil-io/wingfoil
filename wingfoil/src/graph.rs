use crate::queue::HashByRef;
use crate::queue::TimeQueue;
use crate::types::{NanoTime, Node};

use crossbeam::channel::{Receiver, SendError, Sender, select};
use lazy_static::lazy_static;
use std::cmp::{max, min};
use std::collections::HashMap;
use std::convert::TryInto;
use std::fs::File;
use std::io::{Error, Write};
use std::path::Path;
use std::rc::Rc;
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
    is_last_cycle: bool,
    current_node_index: Option<usize>,
    scheduled_callbacks: TimeQueue<usize>,
    always_callbacks: Vec<usize>,
    result: Option<anyhow::Result<()>>,
    node_to_index: HashMap<HashByRef<dyn Node>, usize>,
    node_ticked: Vec<bool>,
    run_time: Arc<tokio::runtime::Runtime>,
    run_mode: RunMode,
    run_for: RunFor,
    ready_notifier: Sender<usize>,
    ready_callbacks: Receiver<usize>,
    start_time: NanoTime,
    id: usize,
    nodes: Vec<NodeData>,
    dirty_nodes_by_layer: Vec<Vec<usize>>,
    node_dirty: Vec<bool>,
}

impl GraphState {
    pub fn new(
        run_time: Arc<tokio::runtime::Runtime>,
        run_mode: RunMode,
        run_for: RunFor,
        start_time: NanoTime,
    ) -> Self {
        let (ready_notifier, ready_callbacks) = crossbeam::channel::unbounded();
        let mut id = GRAPH_ID.lock().unwrap();
        let slf = Self {
            time: NanoTime::ZERO,
            is_last_cycle: false,
            current_node_index: None,
            scheduled_callbacks: TimeQueue::new(),
            always_callbacks: Vec::new(),
            result: None,
            node_to_index: HashMap::new(),
            node_ticked: Vec::new(),
            run_time,
            ready_notifier,
            run_mode,
            run_for,
            ready_callbacks,
            start_time,
            id: *id,
            nodes: Vec::new(),
            dirty_nodes_by_layer: Vec::new(),
            node_dirty: Vec::new(),
        };
        *id += 1;
        slf
    }

    /// The current engine time
    pub fn time(&self) -> NanoTime {
        self.time
    }

    /// The current engine time
    pub fn elapsed(&self) -> NanoTime {
        self.time - self.start_time
    }

    pub fn start_time(&self) -> NanoTime {
        self.start_time
    }

    pub(crate) fn ready_notifier(&self) -> ReadyNotifier {
        ReadyNotifier {
            node_index: self.current_node_index.unwrap(),
            sender: self.ready_notifier.clone(),
        }
    }

    pub fn tokio_runtime(&self) -> Arc<tokio::runtime::Runtime> {
        self.run_time.clone()
    }

    pub fn add_callback(&mut self, time: NanoTime) {
        self.add_callback_for_node(self.current_node_index.unwrap(), time);
    }

    pub fn always_callback(&mut self) {
        let ix = self.current_node_index.unwrap();
        self.always_callbacks.push(ix);
    }

    pub fn is_last_cycle(&self) -> bool {
        self.is_last_cycle
    }

    /// Returns true if node has ticked on the current engine cycle
    pub fn ticked(&self, node: Rc<dyn Node>) -> bool {
        self.node_ticked[self.node_index(node).unwrap()]
    }

    pub fn terminate(&mut self, result: anyhow::Result<()>) {
        self.result = Some(result)
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
        if self.scheduled_callbacks.is_empty() {
            NanoTime::MAX
        } else {
            self.scheduled_callbacks.next_time()
        }
    }

    fn add_callback_for_node(&mut self, node_index: usize, time: NanoTime) {
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
                    Some(msg.unwrap())
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
        if log_enabled!(level) && let Some(ix) = self.current_node_index {
            let id = self.id;
            let type_name = &self.nodes[ix].node.type_name();
            log!(target: type_name, level, "[{id:},{ix:}]{msg:}");
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
        //let cores = core_affinity::get_core_ids().unwrap();
        //core_affinity::set_for_current(cores[0]);
        let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
            // .worker_threads(4)
            // .thread_name("worker")
            // .on_thread_start(move || {
            //     let thread_index = std::thread::current().name()
            //         .unwrap()
            //         .trim_start_matches("worker")
            //         .parse::<usize>()
            //         .unwrap_or(0);
            //     core_affinity::set_for_current(cores[(thread_index + 1) % cores.len()]);
            // })
            .enable_all()
            .build()
            .unwrap();
        let state = GraphState::new(Arc::new(tokio_runtime), run_mode, run_for, NanoTime::ZERO);
        let mut graph = Graph { state };
        graph.initialise(root_nodes);
        graph
    }

    pub fn new_with(
        root_nodes: Vec<Rc<dyn Node>>,
        tokio_runtime: Arc<tokio::runtime::Runtime>,
        run_mode: RunMode,
        run_for: RunFor,
        start_time: NanoTime,
    ) -> Graph {
        let state = GraphState::new(tokio_runtime, run_mode, run_for, start_time);

        let mut graph = Graph { state };
        graph.initialise(root_nodes);
        graph
    }

    pub(crate) fn setup_nodes(&mut self) {
        self.apply_nodes("setup", |node, state| {
            node.setup(state);
        });
    }

    pub(crate) fn start_nodes(&mut self) {
        self.apply_nodes("start", |node, state| {
            node.start(state);
        });
    }

    pub(crate) fn stop_nodes(&mut self) {
        self.apply_nodes("stop", |node, state| {
            node.stop(state);
        });
    }

    pub(crate) fn teardown_nodes(&mut self) {
        self.apply_nodes("teardown", |node, state| {
            node.teardown(state);
        });
    }

    fn apply_nodes(&mut self, desc: &str, func: impl Fn(Rc<dyn Node>, &mut GraphState)) {
        //println!("*** {:}graph {:} {:}", "   ".repeat(self.state.id), self.state.id, desc);
        let timer = Instant::now();
        for ix in 0..self.state.nodes.len() {
            let node = &self.state.nodes[ix];
            self.state.current_node_index = Some(ix);
            func(node.node.clone(), &mut self.state);
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
            if self.state.result.is_some() {
                return self.state.result.take().unwrap();
            }
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
            self.cycle();
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

    pub fn run(&mut self) -> anyhow::Result<()> {
        self.setup_nodes();
        self.start_nodes();
        self.run_nodes()?;
        self.stop_nodes();
        self.teardown_nodes();
        Ok(())
    }

    fn initialise(&mut self, root_nodes: Vec<Rc<dyn Node>>) -> &mut Graph {
        let timer = Instant::now();
        for node in root_nodes {
            if !self.state.seen(node.clone()) {
                self.initialise_node(&node);
            }
        }
        let mut max_layer: i32 = -1;
        for i in 0..self.state.nodes.len() {
            max_layer = max(max_layer, self.state.nodes[i].layer.try_into().unwrap());
            self.state.node_dirty.push(false);
            for j in 0..self.state.nodes[i].upstreams.len() {
                let (up_index, active) = self.state.nodes[i].upstreams[j];
                self.state.nodes[up_index].downstreams.push((i, active));
            }
        }
        for _ in 0..max_layer + 1 {
            self.state.dirty_nodes_by_layer.push(vec![]);
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
        if self.state.seen(node.clone()) {
            self.state.node_index(node.clone()).unwrap()
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
            let ix = self.state.scheduled_callbacks.pop();
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
            self.state.time = self.state.next_scheduled_time();
        }
        self.process_scheduled_callbacks()
    }

    fn process_ready_callbacks(&mut self) -> bool {
        let mut progressed = false;
        while !self.state.ready_callbacks.is_empty() {
            let ix = self.state.ready_callbacks.recv().unwrap();
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
        self.state.time = NanoTime::now();
        progressed
    }

    fn cycle(&mut self) {
        for lyr in 0..self.state.dirty_nodes_by_layer.len() {
            for i in 0..self.state.dirty_nodes_by_layer[lyr].len() {
                let ix = self.state.dirty_nodes_by_layer[lyr][i];
                self.cycle_node(ix);
            }
        }
        self.reset();
    }

    fn cycle_node(&mut self, index: usize) {
        let node = &self.state.nodes[index].node;
        self.state.current_node_index = Some(index);
        let ticked = node.clone().cycle(&mut self.state);
        self.state.current_node_index = None;
        //trace!("cycled node [{:02}] Ticked =  {:?} => {:}", index, ticked, node);
        if ticked {
            self.state.set_ticked(index);
            for i in 0..self.state.nodes[index].downstreams.len() {
                let (downstream_index, active) = self.state.nodes[index].downstreams[i];
                if active {
                    self.mark_dirty(downstream_index)
                }
            }
        }
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
            .tree_fold1(
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
        println!("captured_data  {:?}", captured_data);
        println!("expected       {:?}", expected);
        println!();
        assert_eq!(captured_data, expected);
    }

    fn push_all(inputs: &[Rc<RefCell<CallBackStream<i32>>>], value_at: ValueAt<i32>) {
        inputs
            .iter()
            .for_each(|input| input.borrow_mut().push(value_at.clone()));
    }

    fn push_first(inputs: &[Rc<RefCell<CallBackStream<i32>>>], value_at: ValueAt<i32>) {
        inputs[0].borrow_mut().push(value_at);
    }
}

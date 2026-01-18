use log::Level::Info;
use std::rc::Rc;
use std::thread;
use std::time::Duration;
use tinyvec::TinyVec;
use wingfoil::*;

fn label(name: &str) -> String {
    format!("{:?} >> {:9}", thread::current().id(), name)
}

fn main() {
    env_logger::init();
    let period = Duration::from_millis(100);
    let run_mode = RunMode::RealTime;
    let run_for = RunFor::Duration(period * 6);

    let produce_graph = move || {
        let label = label("producer");
        ticker(period).count().logged(&label, Info)
    };

    let map_graph = |src: Rc<dyn Stream<TinyVec<[u64; 1]>>>| {
        let label = label("mapper");
        src.collapse().map(|x| x * 10).logged(&label, Info)
    };

    producer(produce_graph)
        .collapse()
        .logged(&label("main-pre"), Info)
        .mapper(map_graph)
        .collapse()
        .logged(&label("main-post"), Info)
        .run(run_mode, run_for)
        .unwrap();
}

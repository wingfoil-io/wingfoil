//! Compile a graph's schedule to Rust with `wingfoil::codegen`.
//!
//! Wires the odds/evens graph from the crate docs and emits a standalone
//! static runner for it. Prints the generated source to stdout, or writes it
//! to a file if a path is given:
//!
//! ```sh
//! cargo run --example codegen                    # print to stdout
//! cargo run --example codegen -- src/generated.rs
//! ```
//!
//! The emitted file exposes `pub fn run(roots, run_mode, run_for)` — declare
//! it as a module in your crate and call it with root nodes produced by the
//! same wiring code:
//!
//! ```ignore
//! mod generated;
//! generated::run(vec![wire()], RunMode::RealTime, RunFor::Forever)?;
//! ```

use std::rc::Rc;
use std::time::Duration;
use wingfoil::codegen::{CodegenOptions, generate};
use wingfoil::*;

/// The same wiring feeds both engines: `Graph::run` interprets it, while the
/// generated runner executes it through the compiled schedule.
fn wire() -> Rc<dyn Node> {
    let period = Duration::from_millis(10);
    let source = ticker(period).count();
    let is_even = source.map(|i| i % 2 == 0);
    let odds = source.filter(is_even.not()).map(|i| format!("{i} is odd"));
    let evens = source.filter(is_even).map(|i| format!("{i} is even"));
    merge(vec![odds, evens]).print().as_node()
}

fn main() -> anyhow::Result<()> {
    let source = generate(vec![wire()], &CodegenOptions::default())?;
    match std::env::args().nth(1) {
        Some(path) => {
            std::fs::write(&path, &source)?;
            println!("wrote generated static runner to {path}");
        }
        None => print!("{source}"),
    }
    Ok(())
}

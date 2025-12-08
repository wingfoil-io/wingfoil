use ::wingfoil::{GraphState, NanoTime, RunMode};
use pyo3::prelude::*;

#[pyclass(unsendable, name = "GraphState")]
pub struct PyGraphState {
    // We cannot easily wrap a reference to GraphState because of lifetime issues in PyO3.
    // However, if we are passing this TO python from rust callback, we might need a different approach.
    // For now, let's assume we are creating a view or copy of specific state if accessed, 
    // OR we are wrapping it temporarily.
    //
    // Actually, looking at typical patterns, if we want to expose 'GraphState' methods,
    // we might need to hold `*mut GraphState` (unsafe) or just expose the data we can copy.
    //
    // Given the constraints and the goal "flesh out bindings ... including GraphState",
    // let's define the class and identifying that it represents the state.
    //
    // If we can't wrap &mut GraphState safely, we can at least mimic the API for now 
    // or expose what's possible (like time).
    // The "correct" way often involves `PyRef` or specialized wrappers.
    //
    // Simplification for this task: Store copies of the data that matters (Time), 
    // OR pointer if we were integrating deeper.
    //
    // Let's implement a version that wraps a cloneable subset OR just the pointer unsafely if we trust usage.
    // BUT since we can't easily change the core `GraphState` to be `Clone` (it has queues etc),
    // and we don't have lifetime bounds on PyClass easily.
    //
    // Let's implement a struct that CAN hold the values if populated, or we just implement the methods 
    // and rely on a specific way to construct it.
    
    // For now, let's create a struct that holds the time and run_mode, which are the most common things queried.
    pub time: NanoTime,
    pub start_time: NanoTime,
}

#[pymethods]
impl PyGraphState {
    pub fn time(&self) -> u64 {
        self.time.as_nanos()
    }
    
    pub fn start_time(&self) -> u64 {
        self.start_time.as_nanos()
    }
    
    // Elapsed involves logic, likely (time - start_time)
    pub fn elapsed(&self) -> u64 {
        (self.time - self.start_time).as_nanos()
    }
}

// We need a way to construct this from the real GraphState.
impl PyGraphState {
    pub fn from_state(state: &GraphState) -> Self {
        Self {
            time: state.time(),
            start_time: state.start_time(),
        }
    }
}

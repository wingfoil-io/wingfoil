

## Graph Execution

In this example we illustrate the power of wingfoil's breadth-first graph execution
algorithm and compare with ReactiveX's depth-first approach.   Depth first execution
is also problematic for async streams.

Also note that wingfoil's depth first approach, by constsuction, eliminates
"reactive glitches" (potential logic defects due to inconsistent intermediate state).
See [StackOverflow](https://stackoverflow.com/questions/25139257/terminology-what-is-a-glitch-in-functional-reactive-programming-rx)
and [Wikipedia](https://en.wikipedia.org/wiki/Reactive_programming#Glitches)
for more details.

In wingfoil we build an example with a depth of 127 branch / recombine operations:
```rust
use wingfoil::*;

fn main(){
    let mut source = constant(1_u128);
    for _ in 0..127 {
        source = add(&source, &source);
    }
    let cycles = source.count();
    cycles.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    println!("cycles {:?}", cycles.peek_value());
    println!("value {:?}", source.peek_value());
}
```
It produces the correct ouput of 2^127 in 1 engine cycle that takes
less than half a millisecond to complete.
```pre
384 nodes wired in 212.904µs
Completed 1 cycles in 463.793µs. 463.684µs average.
cycles 1
value 170141183460469231731687303715884105728
````

If we try and implement the same logic in ReactiveX we quickly realise
that it is unfeasible.   The issue is that each branch / recombine operation
doubles the number of downstream ticks, resulting in an explosive O(2^N)
time complexity.  Extrapolating from a smaller depths, we can
estimate that it would of the order of 10^24 years to run the example with a
depth of 127.


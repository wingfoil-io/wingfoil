# Benchmarks

Here is some sample output from the [10x10](graph.rs) benchmark.

In this benchmark we measure graph overhead by wiring up a trivial graph, 10 nodes deep and 10 nodes 
wide, with all nodes ticking on each engine cycle.  Running on 3.80 GHz CPU, we observe a latency 
around 2µs per engine cycle, equivalent to 20ns per node cycle.   For a smaller graph of 10 nodes, 
this would give a graph overhead around 200ns per engine cycle.

<img src="images/pdf.png" width="600">
<img src="images/regression.png" width="600">

## Additional Statistics

| Metric | Lower bound | Estimate | Upper bound |
|--------|------------|----------|------------|
| Slope  | 1.9909 µs  | 1.9912 µs | 1.9914 µs |
| R²     | 0.9999278  | 0.9999302 | 0.9999277 |
| Mean   | 1.9911 µs  | 1.9916 µs | 1.9923 µs |
| Std. Dev. | 1.3205 ns | 2.9944 ns | 4.3582 ns |
| Median | 1.9907 µs  | 1.9911 µs | 1.9914 µs |
| MAD    | 985.78 ps  | 1.2937 ns | 1.6244 ns |

## Additional Plots

- [Typical](images/typical.png)
- [Mean](images/mean.png)
- [Std. Dev.](images/SD.png)
- [Median](images/median.png)
- [MAD](images/MAD.png)
- [Slope](images/slope.png)

## Understanding this report

The first plot displays the average time per iteration for this benchmark. The shaded region
shows the estimated probability of an iteration taking a certain amount of time, while the line
shows the mean

The second plot show the linear regression calculated from the measurements. Each point
represents a sample, though here it shows the total time for the sample rather than time per
iteration. The line is the line of best fit for these measurements.

See [Criterion.rs documentation](https://bheisler.github.io/criterion.rs/book/user_guide/command_line_output.html#additional-statistics) for more details on the additional statistics.

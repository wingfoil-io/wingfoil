# 10x10 - Criterion.rs Benchmark

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

- [Typical](typical.svg)
- [Mean](mean.svg)
- [Std. Dev.](SD.svg)
- [Median](median.svg)
- [MAD](MAD.svg)
- [Slope](slope.svg)

## Understanding this report

The plot on the left displays the average time per iteration for this benchmark. The shaded region
shows the estimated probability of an iteration taking a certain amount of time, while the line
shows the mean. Click on the plot for a larger view showing the outliers.

The plot on the right shows the linear regression calculated from the measurements. Each point
represents a sample, though here it shows the total time for the sample rather than time per
iteration. The line is the line of best fit for these measurements.

See [Criterion.rs documentation](https://bheisler.github.io/criterion.rs/book/user_guide/command_line_output.html#additional-statistics) for more details on the additional statistics.

# Renders latency.png for the topological-sort vs per-path-propagation
# branch/recombine comparison.
#
# The three arrays below are *readings*, not source: refill them from a local
# run before regenerating the plot, since criterion wall-clock numbers are
# hardware-specific.
#
#   cargo bench -p wingfoil-next --features bench --bench bfs_vs_dfs_wingfoil
#   cargo bench -p wingfoil-next --bench bfs_vs_dfs_reactive
#   cargo bench -p wingfoil-next --features async --bench bfs_vs_dfs_async_streams
#   python plot.py
#
# Read the numbers off the criterion *console output* of each run, not out of
# `target/criterion/`: all three targets name their benchmarks `depth_1`..
# `depth_10`, so whichever ran last owns those directories on disk.
#
# The values in place are a next-engine reading — all three series measured
# back to back on the machine described in `../images/lscpu.txt` (4-core
# 2.80 GHz Xeon VM). Point estimates, in nanoseconds.
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import matplotlib.ticker as ticker

depths = list(range(1, 11))

wingfoil = [610, 410, 446, 438, 539, 513, 562, 538, 575, 681]
async_s  = [188, 309, 494, 907, 1782, 3394, 6847, 13405, 30360, 54872]
reactive = [46,  109, 257, 517, 1140, 2147, 4333, 8452, 17437, 40286]

fig, ax = plt.subplots(figsize=(8, 5))

ax.plot(depths, wingfoil, 'o-', color='#2196F3', linewidth=2, markersize=6, label='wingfoil (topologically sorted)')
ax.plot(depths, async_s,  's-', color='#FF9800', linewidth=2, markersize=6, label='async streams (per-path)')
ax.plot(depths, reactive, '^-', color='#F44336', linewidth=2, markersize=6, label='reactive / rxrust (per-path)')

plt.yscale('log')

ax.yaxis.set_major_locator(ticker.LogLocator(base=10))
ax.yaxis.set_minor_locator(ticker.LogLocator(base=10, subs=[2, 3, 4, 5, 6, 7, 8, 9]))

def fmt_time(y, _):
    return f'{y:.0f} ns' if y < 1000 else f'{y/1000:.0f} µs'

ax.yaxis.set_major_formatter(ticker.FuncFormatter(fmt_time))
ax.yaxis.set_minor_formatter(ticker.NullFormatter())

ax.grid(True, which='major', linestyle='-',  linewidth=0.8, alpha=0.6)
ax.grid(True, which='minor', linestyle='--', linewidth=0.5, alpha=0.4)
ax.set_axisbelow(True)

ax.set_xticks(depths)
ax.set_xlabel('Branch/recombine depth', fontsize=12)
ax.set_ylabel('Latency per tick', fontsize=12)
ax.set_title('Topological sort vs per-path propagation: branch/recombine latency', fontsize=13, fontweight='bold')
ax.legend(fontsize=11)

fig.tight_layout()
fig.savefig('latency.png', dpi=150, bbox_inches='tight')
print("saved")

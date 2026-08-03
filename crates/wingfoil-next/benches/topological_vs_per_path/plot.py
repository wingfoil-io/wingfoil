# Renders latency.png and per_cycle.png for the topological-sort vs
# per-path-propagation branch/recombine comparison.
#
# The arrays below are *readings*, not source: refill them from a local run
# before regenerating the plots, since criterion wall-clock numbers are
# hardware-specific.
#
#   cargo bench -p wingfoil-next --features bench --bench bfs_vs_dfs_wingfoil
#   cargo bench -p wingfoil-next --bench bfs_vs_dfs_reactive
#   cargo bench -p wingfoil-next --features async --bench bfs_vs_dfs_async_streams
#   python plot.py
#
# Read the numbers off the criterion *console output* of each run, not out of
# `target/criterion/`: all three targets name their per-tick benchmarks
# `depth_1`..`depth_10`, so whichever ran last owns those directories on disk.
#
# The wingfoil target emits four series from one set of `nitro!` blocks:
# `depth_N` / `depth_N_nested` are per *tick* through the bench handshake (the
# measurement the other two libraries also make), and `cycles_depth_N/*` are per
# *cycle* over a fixed 10 000-cycle run with no handshake under them — whole-run
# time divided by 10 000 goes in the second pair of arrays.
#
# The values in place are a next-engine reading — every series measured back to
# back on the machine described in `../images/lscpu-b.txt` (4-core 2.10 GHz
# Xeon VM). Point estimates, in nanoseconds.
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import matplotlib.ticker as ticker

depths = list(range(1, 11))

# Per tick, through the bench harness handshake.
wingfoil = [323, 567, 507, 509, 446, 441, 454, 683, 533, 591]
nested   = [570, 330, 437, 469, 269, 377, 383, 333, 436, 353]
async_s  = [142, 184, 385, 659, 1117, 2439, 4640, 10185, 17658, 33750]
reactive = [21, 69, 153, 298, 692, 1326, 2748, 5620, 10490, 23705]

# Per cycle, harness divided out (whole-run time / 10 000 cycles).
cyc_interp = [87, 91, 116, 156, 144, 185, 210, 221, 269, 267]
cyc_nested = [83, 86, 83, 88, 86, 85, 83, 75, 86, 95]

INTERP_COLOR = '#2196F3'
ISLAND_COLOR = '#0D47A1'


def style(ax, ylabel, title, legend_size=11):
    ax.grid(True, which='major', linestyle='-', linewidth=0.8, alpha=0.6)
    ax.grid(True, which='minor', linestyle='--', linewidth=0.5, alpha=0.4)
    ax.set_axisbelow(True)
    ax.set_xticks(depths)
    ax.set_xlabel('Branch/recombine depth', fontsize=12)
    ax.set_ylabel(ylabel, fontsize=12)
    ax.set_title(title, fontsize=13, fontweight='bold')
    ax.legend(fontsize=legend_size)


def fmt_time(y, _):
    return f'{y:.0f} ns' if y < 1000 else f'{y/1000:.0f} µs'


# --- Chart 1: the cross-library comparison, per tick, log scale -------------
fig, ax = plt.subplots(figsize=(8, 5))

ax.plot(depths, wingfoil, 'o-', color=INTERP_COLOR, linewidth=2, markersize=6,
        label='wingfoil interpreted (topologically sorted)')
ax.plot(depths, nested, 'D--', color=ISLAND_COLOR, linewidth=2, markersize=5,
        label='wingfoil compiled island (topologically sorted)')
ax.plot(depths, async_s, 's-', color='#FF9800', linewidth=2, markersize=6,
        label='async streams (per-path)')
ax.plot(depths, reactive, '^-', color='#F44336', linewidth=2, markersize=6,
        label='reactive / rxrust (per-path)')

plt.yscale('log')
ax.yaxis.set_major_locator(ticker.LogLocator(base=10))
ax.yaxis.set_minor_locator(ticker.LogLocator(base=10, subs=[2, 3, 4, 5, 6, 7, 8, 9]))
ax.yaxis.set_major_formatter(ticker.FuncFormatter(fmt_time))
ax.yaxis.set_minor_formatter(ticker.NullFormatter())

style(ax, 'Latency per tick', 'Topological sort vs per-path propagation: branch/recombine latency', 10)
fig.tight_layout()
fig.savefig('latency.png', dpi=150, bbox_inches='tight')

# --- Chart 2: wingfoil alone, per cycle, linear scale -----------------------
#
# The same graphs with the harness handshake divided out, on a linear axis: this
# is the O(N) claim itself — one more level is one more node, a fixed step up,
# not a doubling. (Compare the log axis above, where the per-path libraries need
# four decades.) The island line is flatter still: its added node is compiled
# straight-line code, so it costs ~0.3 ns rather than the interpreter's ~22.
fig2, ax2 = plt.subplots(figsize=(8, 5))

ax2.plot(depths, cyc_interp, 'o-', color=INTERP_COLOR, linewidth=2, markersize=6,
         label='wingfoil interpreted')
ax2.plot(depths, cyc_nested, 'D--', color=ISLAND_COLOR, linewidth=2, markersize=5,
         label='wingfoil compiled island')

ax2.set_ylim(bottom=0)
ax2.yaxis.set_major_formatter(ticker.FuncFormatter(fmt_time))

style(ax2, 'Cost per cycle (10 000-cycle run)',
      'Branch/recombine cost per cycle, bench harness divided out')
fig2.tight_layout()
fig2.savefig('per_cycle.png', dpi=150, bbox_inches='tight')

print("saved")

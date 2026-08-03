# Renders latency.png, per_cycle.png and cross_library.png for the
# topological-sort vs per-path-propagation branch/recombine comparison.
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
# The wingfoil target emits five series from two sets of `nitro!` blocks:
# `depth_N` / `depth_N_nested` are per *tick* through the bench handshake (the
# measurement the other two libraries also make), and
# `cycles_depth_N/{interpreted,compiled,nested}` are per *cycle* over a fixed
# 10 000-cycle run with no handshake under them — whole-run time divided by
# 10 000 goes in the second group of arrays.
#
# The values in place are a next-engine reading — every series measured back to
# back on the machine described in `../images/lscpu-b.txt` (4-core 2.10 GHz
# Xeon VM). Point estimates, in nanoseconds.
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import matplotlib.ticker as ticker

depths = list(range(1, 11))

# Per tick, through the bench harness handshake. The wingfoil pair carries a
# criterion<->worker handshake that rxrust and tokio do not pay; see the README.
wingfoil = [400, 494, 383, 365, 429, 491, 422, 539, 505, 582]
nested   = [415, 300, 427, 439, 522, 579, 391, 320, 386, 558]
async_s  = [152, 233, 364, 693, 1263, 2509, 5100, 9996, 19869, 38487]
reactive = [24, 65, 167, 292, 672, 1374, 2820, 5727, 11266, 22595]

# Per cycle, harness divided out (whole-run time / 10 000 cycles).
cyc_interp   = [87.0, 116.4, 135.5, 150.3, 179.7, 199.0, 217.5, 259.0, 257.4, 287.5]
cyc_compiled = [21.1, 22.2, 21.9, 23.7, 23.3, 24.5, 24.7, 23.5, 25.2, 23.9]
cyc_nested   = [73.8, 78.1, 80.1, 73.4, 78.6, 74.6, 81.8, 72.7, 80.5, 86.0]

INTERP_COLOR   = '#2196F3'
ISLAND_COLOR   = '#0D47A1'
COMPILED_COLOR = '#00897B'
ASYNC_COLOR    = '#FF9800'
RX_COLOR       = '#F44336'


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


def log_axis(ax):
    plt.yscale('log')
    ax.yaxis.set_major_locator(ticker.LogLocator(base=10))
    ax.yaxis.set_minor_locator(ticker.LogLocator(base=10, subs=[2, 3, 4, 5, 6, 7, 8, 9]))
    ax.yaxis.set_major_formatter(ticker.FuncFormatter(fmt_time))
    ax.yaxis.set_minor_formatter(ticker.NullFormatter())


# --- Chart 1: the cross-library comparison, per tick, log scale -------------
#
# All four series are one tick through their own harness. The wingfoil pair is
# the only one paying a cross-thread handshake, which is why they sit a few
# hundred ns up and read as flat well before the graph is: see the README.
fig, ax = plt.subplots(figsize=(8, 5))

ax.plot(depths, wingfoil, 'o-', color=INTERP_COLOR, linewidth=2, markersize=6,
        label='wingfoil interpreted (topologically sorted)')
ax.plot(depths, nested, 'D--', color=ISLAND_COLOR, linewidth=2, markersize=5,
        label='wingfoil compiled island (topologically sorted)')
ax.plot(depths, async_s, 's-', color=ASYNC_COLOR, linewidth=2, markersize=6,
        label='async streams (per-path)')
ax.plot(depths, reactive, '^-', color=RX_COLOR, linewidth=2, markersize=6,
        label='reactive / rxrust (per-path)')

log_axis(ax)
style(ax, 'Latency per tick', 'Topological sort vs per-path propagation: branch/recombine latency', 10)
fig.tight_layout()
fig.savefig('latency.png', dpi=150, bbox_inches='tight')

# --- Chart 2: the three wingfoil tiers, per cycle, linear scale -------------
#
# The same graphs with the harness handshake divided out, on a linear axis: this
# is the O(N) claim itself — one more level is one more node, a fixed step up,
# not a doubling. (Compare the log axis above, where the per-path libraries need
# four decades.) Both compiled tiers are flat: their added node is straight-line
# code, so it costs about what the arithmetic costs rather than the
# interpreter's ~22 ns of dispatch.
fig2, ax2 = plt.subplots(figsize=(8, 5))

ax2.plot(depths, cyc_interp, 'o-', color=INTERP_COLOR, linewidth=2, markersize=6,
         label='wingfoil interpreted')
ax2.plot(depths, cyc_nested, 'D--', color=ISLAND_COLOR, linewidth=2, markersize=5,
         label='wingfoil compiled island (nested)')
ax2.plot(depths, cyc_compiled, 'v-', color=COMPILED_COLOR, linewidth=2, markersize=6,
         label='wingfoil compiled (whole program)')

ax2.set_ylim(bottom=0)
ax2.yaxis.set_major_formatter(ticker.FuncFormatter(fmt_time))

style(ax2, 'Cost per cycle (10 000-cycle run)',
      'Branch/recombine cost per cycle, bench harness divided out')
fig2.tight_layout()
fig2.savefig('per_cycle.png', dpi=150, bbox_inches='tight')

# --- Chart 3: cross-library, with the wingfoil handshake removed ------------
#
# Chart 1 is the measurement the three targets actually make, and it is
# conservative: only the wingfoil series pays a handshake. This chart puts the
# harness-free wingfoil tiers against the same two baselines, which is the
# closest like-for-like available — the baselines have no handshake to remove,
# and wingfoil's has been divided out. Mixed harnesses, so read the *slopes*;
# the ratios are quoted in the README with that caveat attached.
fig3, ax3 = plt.subplots(figsize=(8, 5))

ax3.plot(depths, cyc_interp, 'o-', color=INTERP_COLOR, linewidth=2, markersize=6,
         label='wingfoil interpreted (per cycle)')
ax3.plot(depths, cyc_nested, 'D--', color=ISLAND_COLOR, linewidth=2, markersize=5,
         label='wingfoil compiled island (per cycle)')
ax3.plot(depths, cyc_compiled, 'v-', color=COMPILED_COLOR, linewidth=2, markersize=6,
         label='wingfoil compiled (per cycle)')
ax3.plot(depths, async_s, 's-', color=ASYNC_COLOR, linewidth=2, markersize=6,
         label='async streams (per tick)')
ax3.plot(depths, reactive, '^-', color=RX_COLOR, linewidth=2, markersize=6,
         label='reactive / rxrust (per tick)')

log_axis(ax3)
style(ax3, 'Cost per tick / cycle',
      'Topological sort vs per-path propagation, wingfoil harness removed', 9)
fig3.tight_layout()
fig3.savefig('cross_library.png', dpi=150, bbox_inches='tight')

print("saved")

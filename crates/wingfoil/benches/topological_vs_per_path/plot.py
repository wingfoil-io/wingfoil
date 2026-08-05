# Renders the charts for the topological-sort vs per-path-propagation
# branch/recombine comparison: headline.png / headline_log.png (the pair the
# benchmark README opens with — same data, linear for the shape and log to read
# the low end), cross_library.png / cross_library_log.png (the same comparison
# with every tier drawn as its own line), and per_cycle.png (the engine's three
# tiers on their own).
#
# The arrays below are *readings*, not source: refill them from a local run
# before regenerating the plots, since criterion wall-clock numbers are
# hardware-specific.
#
#   cargo bench -p wingfoil --bench bfs_vs_dfs_wingfoil
#   cargo bench -p wingfoil --bench bfs_vs_dfs_reactive
#   cargo bench -p wingfoil --features async --bench bfs_vs_dfs_async_streams
#   python plot.py
#
# Read the numbers off the criterion *console output* of each run, not out of
# `target/criterion/`: the reactive and async targets both name their
# benchmarks `depth_1`..`depth_10`, so whichever ran last owns those
# directories on disk. (The wingfoil target names its groups `cycles_depth_N`
# precisely to stay out of that collision.)
#
# The wingfoil target emits three series, one per engine tier, from a single set
# of `nitro!` blocks: `cycles_depth_N/{interpreted,compiled,nested}`, each a
# fixed 10 000-cycle run of a self-contained graph. Whole-run time divided by
# 10 000 is the per-cycle figure that goes in the arrays below. The other two
# targets time one source event per sample, called directly on the criterion
# thread — a different measurement, so read the slopes rather than the ratios.
#
# The values in place are a wingfoil engine reading — every series measured back to
# back on the machine described in `../images/lscpu-b.txt` (4-core 2.10 GHz
# Xeon VM). Point estimates, in nanoseconds.
import os
import sys

import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import matplotlib.ticker as ticker

# The colour language is shared with the rest of the benchmark charts so a
# reader can carry meaning between them — blue is wingfoil, and which blue says
# which tier. See ../palette.py.
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), '..'))
import palette as p

depths = list(range(1, 11))

# Per source event, called directly on the criterion thread. Neither baseline
# has a bench handshake to remove; see the README on what does and does not make
# these comparable to the wingfoil series.
async_s  = [152, 233, 364, 693, 1263, 2509, 5100, 9996, 19869, 38487]
reactive = [24, 65, 167, 292, 672, 1374, 2820, 5727, 11266, 22595]

# wingfoil, per cycle: whole-run time / 10 000 cycles, no harness underneath.
cyc_interp   = [87.0, 116.4, 135.5, 150.3, 179.7, 199.0, 217.5, 259.0, 257.4, 287.5]
cyc_compiled = [21.1, 22.2, 21.9, 23.7, 23.3, 24.5, 24.7, 23.5, 25.2, 23.9]
cyc_nested   = [73.8, 78.1, 80.1, 73.4, 78.6, 74.6, 81.8, 72.7, 80.5, 86.0]


def style(ax, ylabel, title, subtitle=None, legend_size=9.5, legend_loc='upper left'):
    p.style(ax, xlabel='Branch/recombine depth', ylabel=ylabel, title=title,
            subtitle=subtitle)
    ax.set_xticks(depths)
    legend = ax.legend(fontsize=legend_size, loc=legend_loc, frameon=False,
                       labelcolor=p.INK_SOFT)
    return legend


def log_axis(ax):
    ax.set_yscale('log')
    ax.yaxis.set_major_locator(ticker.LogLocator(base=10))
    ax.yaxis.set_minor_locator(ticker.LogLocator(base=10, subs=[2, 3, 4, 5, 6, 7, 8, 9]))
    ax.yaxis.set_major_formatter(ticker.FuncFormatter(p.fmt_time))
    ax.yaxis.set_minor_formatter(ticker.NullFormatter())


def linear_axis(ax):
    ax.set_ylim(bottom=0)
    ax.yaxis.set_major_formatter(ticker.FuncFormatter(p.fmt_time))


# --- Charts 1 & 2: the headline pair ----------------------------------------
#
# What the README opens with, and the one claim the whole suite exists to make:
# the two per-path libraries double every level while every wingfoil tier stays
# flat. The three tiers are drawn as a filled band rather than three competing
# lines, because at this scale the distance between wingfoil and a per-path
# library is the subject and the distance between wingfoil's own tiers is not —
# cross_library*.png below is the same data with each tier on its own line.
def headline(log):
    fig, ax = plt.subplots(figsize=(9, 5.4))

    ax.fill_between(depths, cyc_compiled, cyc_interp, color=p.BAND, zorder=1,
                    label='wingfoil — every tier lies in here')
    ax.plot(depths, async_s, 's-', color=p.TOKIO, linewidth=2, markersize=6,
            zorder=3, label='tokio async streams — per path')
    ax.plot(depths, reactive, '^-', color=p.RXRUST, linewidth=2, markersize=6,
            zorder=3, label='rxrust reactive — per path')
    ax.plot(depths, cyc_interp, 'o-', color=p.INTERPRETED, linewidth=2,
            markersize=5.5, zorder=4, label='wingfoil interpreted')
    ax.plot(depths, cyc_nested, 'D--', color=p.ISLAND, linewidth=2, markersize=5,
            zorder=4, label='wingfoil compiled island')
    ax.plot(depths, cyc_compiled, 'v-', color=p.COMPILED, linewidth=2,
            markersize=5.5, zorder=4, label='wingfoil compiled')

    if log:
        log_axis(ax)
        ax.set_ylim(9, 120_000)
        ax.set_xlim(0.5, 13.9)

        # Selective direct labels: the two lines the story is about, plus the
        # band. Identity is carried by the marker beside the text, not the text.
        ax.text(10.25, async_s[-1], ' tokio', ha='left', va='center', fontsize=10,
                color=p.INK_SOFT)
        ax.text(10.25, reactive[-1], ' rxrust', ha='left', va='center', fontsize=10,
                color=p.INK_SOFT)
        ax.text(10.25, cyc_interp[-1] * 0.98, ' wingfoil', ha='left', va='center',
                fontsize=10, color=p.INK_SOFT, fontweight='bold')

        # The gap at the deepest measured point, as one number.
        ax.annotate('', xy=(12.5, cyc_compiled[-1]), xytext=(12.5, async_s[-1]),
                    arrowprops=dict(arrowstyle='<->', color=p.INK_MUTED, linewidth=1.2))
        ax.text(12.75, (cyc_compiled[-1] * async_s[-1]) ** 0.5, '1610×',
                ha='left', va='center', fontsize=13, fontweight='bold', color=p.INK)
        ax.text(12.75, (cyc_compiled[-1] * async_s[-1]) ** 0.5 / 1.9,
                'compiled\nvs tokio', ha='left', va='center', fontsize=8.5,
                color=p.INK_MUTED)

        ax.scatter([3], [167], s=110, facecolor='none', edgecolor=p.INK_MUTED,
                   linewidth=1.2, zorder=5)
        ax.annotate('rxrust starts ahead —\nthe interpreter passes it here',
                    xy=(3.15, 145), xytext=(4.5, 12.6), ha='left', va='center',
                    fontsize=8.8, color=p.INK_MUTED, style='italic',
                    arrowprops=dict(arrowstyle='->', color=p.INK_MUTED, linewidth=1))

        subtitle = ('Log axis — the only one that shows both a 24 ns cycle and a '
                    '38 µs one. Slopes are the claim.')
    else:
        linear_axis(ax)
        ax.set_xlim(0.5, 12.6)
        ax.text(10.25, async_s[-1], ' tokio', ha='left', va='center', fontsize=10,
                color=p.INK_SOFT)
        ax.text(10.25, reactive[-1], ' rxrust', ha='left', va='center', fontsize=10,
                color=p.INK_SOFT)
        ax.annotate('wingfoil — all three tiers,\nflat on the floor',
                    xy=(9.9, cyc_interp[-1]), xytext=(4.6, 11_500),
                    ha='center', fontsize=9.5, color=p.INK_SOFT,
                    arrowprops=dict(arrowstyle='->', color=p.INK_MUTED, linewidth=1.1))
        subtitle = ('Linear axis — what doubling per level actually looks like '
                    'against a flat line.')

    style(ax, 'Cost per cycle / event',
          'Every level doubles the paths. Only one of these engines notices.',
          subtitle)
    fig.tight_layout()
    fig.savefig(f'headline{"_log" if log else ""}.png', dpi=150, bbox_inches='tight',
                facecolor=p.SURFACE)
    plt.close(fig)


headline(log=False)
headline(log=True)


# --- Chart 3: the three wingfoil tiers, per cycle, linear scale -------------
#
# The engine on its own, on a linear axis: this is the O(N) claim itself — one
# more level is one more node, a fixed step up, not a doubling. (Compare the log
# axis in chart 4, where the per-path libraries need four decades.) Both
# compiled tiers are flat: their added node is straight-line code, so it costs
# about what the arithmetic costs rather than the interpreter's ~22 ns of
# dispatch.
fig2, ax2 = plt.subplots(figsize=(8, 5))

ax2.plot(depths, cyc_interp, 'o-', color=p.INTERPRETED, linewidth=2, markersize=6,
         label='wingfoil interpreted')
ax2.plot(depths, cyc_nested, 'D--', color=p.ISLAND, linewidth=2, markersize=5,
         label='wingfoil compiled island (nested)')
ax2.plot(depths, cyc_compiled, 'v-', color=p.COMPILED, linewidth=2, markersize=6,
         label='wingfoil compiled (whole program)')

linear_axis(ax2)
style(ax2, 'Cost per cycle (10 000-cycle run)',
      'Branch/recombine cost per cycle, all three engine tiers')
fig2.tight_layout()
fig2.savefig('per_cycle.png', dpi=150, bbox_inches='tight', facecolor=p.SURFACE)
plt.close(fig2)


# --- Chart 4: the same tiers against the two per-path baselines -------------
#
# Mixed harnesses by construction: the wingfoil series are per cycle with
# nothing under them, the baselines are per source event called directly on the
# criterion thread. Neither side carries a bench handshake, which is as close to
# like-for-like as these three targets get, but a cycle and an event are still
# different units. Read the *slopes* — linear against doubling is the claim —
# and take the ratios with the caveat the README attaches to them.
def cross_library(log):
    fig, ax = plt.subplots(figsize=(8, 5))
    for ys, marker, color, label in (
        (cyc_interp, 'o-', p.INTERPRETED, 'wingfoil interpreted (per cycle)'),
        (cyc_nested, 'D--', p.ISLAND, 'wingfoil compiled island (per cycle)'),
        (cyc_compiled, 'v-', p.COMPILED, 'wingfoil compiled (per cycle)'),
        (async_s, 's-', p.TOKIO, 'tokio async streams (per event)'),
        (reactive, '^-', p.RXRUST, 'rxrust reactive (per event)'),
    ):
        ax.plot(depths, ys, marker, color=color, linewidth=2,
                markersize=5 if '--' in marker else 6, label=label)
    (log_axis if log else linear_axis)(ax)
    style(ax, 'Cost per cycle / event',
          'Topological sort vs per-path propagation: branch/recombine cost')
    fig.tight_layout()
    fig.savefig(f'cross_library{"_log" if log else ""}.png', dpi=150,
                bbox_inches='tight', facecolor=p.SURFACE)
    plt.close(fig)


cross_library(log=False)
cross_library(log=True)

print("saved")

# Renders images/graph/overhead.png — what one engine cycle through the `graph`
# bench's 10×10 DAG actually costs, and where that time goes.
#
# The section this feeds used to lead on criterion's own PDF and regression
# plots, which show the *distribution* of a number the reader has not been given
# yet. This chart gives the number instead: the shape of the graph on the left,
# and on the right the measured cycle split into the part that is engine work
# and the part that is the bench harness — because the `node` row of the same
# bench measures that harness with no graph wired at all, and subtracting it is
# what turns 2.709 µs into ~20 ns per node cycle.
#
# The values below are a *reading*, not source: refill them from a local run
# before regenerating, since criterion wall-clock numbers are hardware-specific.
#
#   cargo bench --manifest-path crates/wingfoil/Cargo.toml --features bench --bench graph
#   python plot_graph_overhead.py
#
# In place: the machine-A capture described in README.md (4-core 2.80 GHz Xeon
# KVM guest, images/lscpu.txt). Point estimates, nanoseconds per engine cycle.
import matplotlib

matplotlib.use('Agg')
import matplotlib.pyplot as plt
import matplotlib.ticker as ticker

import palette as p

HARNESS = 677.09          # `node`: no graph wired — criterion<->worker handshake
CYCLE_10X10 = 2709.0      # `10x10`: 102 nodes, every one ticking every cycle
CYCLE_100X100 = 627470.0  # `100x100`: 10 002 nodes

WIDTH, DEPTH = 10, 10
NODES_10X10 = WIDTH * DEPTH
NODES_100X100 = 100 * 100

per_node_10x10 = (CYCLE_10X10 - HARNESS) / NODES_10X10
per_node_100x100 = (CYCLE_100X100 - HARNESS) / NODES_100X100

fig, (ax_dag, ax_cost) = plt.subplots(
    1, 2, figsize=(11, 4.4), gridspec_kw={'width_ratios': [1, 1.45]}
)

# --- left: the graph being measured -----------------------------------------
#
# Drawn to the bench's actual topology (see graph.rs): one `count` source fanned
# into `width` independent chains of `depth` identity maps, recombined by a
# single n-ary merge. Every node ticks on every cycle, which is what makes the
# per-node figure meaningful.
xs = [1 + i for i in range(DEPTH)]
ys = [WIDTH - 1 - r for r in range(WIDTH)]

for r, y in enumerate(ys):
    ax_dag.plot([0, xs[0]], [ys[WIDTH // 2], y], color=p.GRID, linewidth=0.6, zorder=1)
    ax_dag.plot([xs[-1], DEPTH + 1], [y, ys[WIDTH // 2]], color=p.GRID, linewidth=0.6, zorder=1)
    ax_dag.plot(xs, [y] * DEPTH, color=p.GRID, linewidth=0.8, zorder=1)
    ax_dag.scatter(xs, [y] * DEPTH, s=26, color=p.WINGFOIL, zorder=2, linewidths=0)

ax_dag.scatter([0], [ys[WIDTH // 2]], s=90, color=p.COMPILED, zorder=3, linewidths=0)
ax_dag.scatter([DEPTH + 1], [ys[WIDTH // 2]], s=90, color=p.COMPILED, zorder=3, linewidths=0)
ax_dag.text(-0.5, ys[WIDTH // 2], 'count\nsource', ha='right', va='center',
            fontsize=9, color=p.INK_SOFT)
ax_dag.text(DEPTH + 1.5, ys[WIDTH // 2], 'merge', ha='left', va='center',
            fontsize=9, color=p.INK_SOFT)
ax_dag.text(DEPTH / 2 + 0.5, -1.2, '10 chains × 10 maps — 102 nodes, all ticking every cycle',
            ha='center', va='top', fontsize=10, color=p.INK_MUTED)

ax_dag.set_xlim(-3.2, DEPTH + 4.0)
ax_dag.set_ylim(-2.4, WIDTH + 0.4)
ax_dag.axis('off')
ax_dag.set_title('The `10x10` workload', fontsize=13, fontweight='bold',
                 color=p.INK, loc='left')

# --- right: where the measured cycle goes -----------------------------------
bar_y = [1.0, 0.35]
bar_h = 0.30

ax_cost.barh(bar_y[0], HARNESS, height=bar_h, color=p.NEUTRAL, linewidth=0, zorder=2)
ax_cost.barh(bar_y[0], CYCLE_10X10 - HARNESS, left=HARNESS + 12, height=bar_h,
             color=p.WINGFOIL, linewidth=0, zorder=2)
ax_cost.barh(bar_y[1], HARNESS, height=bar_h, color=p.NEUTRAL, linewidth=0, zorder=2)

ax_cost.text(HARNESS / 2, bar_y[0] + bar_h / 2 + 0.06, 'bench harness',
             ha='center', va='bottom', fontsize=9.5, color=p.INK_SOFT)
ax_cost.text(HARNESS + (CYCLE_10X10 - HARNESS) / 2, bar_y[0],
             f'100 map nodes — {CYCLE_10X10 - HARNESS:.0f} ns',
             ha='center', va='center', fontsize=10.5, color='white', fontweight='bold')
ax_cost.text(CYCLE_10X10 + 90, bar_y[0], f'{CYCLE_10X10 / 1000:.2f} µs / cycle',
             ha='left', va='center', fontsize=10.5, color=p.INK)
ax_cost.text(HARNESS + 90, bar_y[1], 'the floor: no graph wired at all',
             ha='left', va='center', fontsize=10.5, color=p.INK_SOFT)

ax_cost.set_yticks(bar_y)
ax_cost.set_yticklabels(['`10x10`', '`node`'], fontsize=11, color=p.INK)
ax_cost.set_ylim(-0.35, 1.65)
ax_cost.set_xlim(0, CYCLE_10X10 * 1.42)
ax_cost.xaxis.set_major_formatter(ticker.FuncFormatter(p.fmt_time))
p.style(ax_cost, xlabel='Time per engine cycle',
        title='Subtract the harness and the engine costs ~20 ns per node',
        subtitle=f'{per_node_10x10:.1f} ns per node cycle at 100 nodes · '
                 f'{per_node_100x100:.1f} ns at 10 000, where the working set stops fitting')
ax_cost.grid(axis='y', visible=False)

fig.tight_layout()
fig.savefig('images/graph/overhead.png', dpi=150, bbox_inches='tight',
            facecolor=p.SURFACE)
plt.close(fig)

print(f'saved images/graph/overhead.png — {per_node_10x10:.1f} ns/node (10x10), '
      f'{per_node_100x100:.1f} ns/node (100x100)')

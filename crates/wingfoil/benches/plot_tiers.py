# Renders images/tiers/summary.png — every `tiers` workload on all four engines,
# twice: what a cycle costs, and how the interpreted tier compares against the
# legacy engine it replaces.
#
# The suite asks two questions of this data and they want different charts. "How
# fast is it?" is an absolute, and spans two decades from a compiled cycle to a
# legacy one, so the left panel is a log dot plot. "Is the port at least as fast
# as the engine it replaces?" is a *relationship*, and criterion's own violin
# plots only ever compare within one group — so the right panel is the ratio
# against legacy, with 1.0 drawn as the line the port has to stay under.
#
# The table below is a *reading*, not source: refill it from a local run
# before regenerating, since criterion wall-clock numbers are hardware
# specific. Milliseconds, point estimates, straight off
#
#   cargo bench -p wingfoil --bench tiers
#
# The values in place were measured on the machine in images/lscpu-b.txt.
import matplotlib

matplotlib.use('Agg')
import matplotlib.pyplot as plt
import matplotlib.ticker as ticker
import numpy as np

import palette as p

# workload -> (legacy, interpreted, compiled, nested), milliseconds
DATA = {
    'dense_chain':  (7.9491,  6.6387,  0.18708, 0.93081),
    'fanout':       (17.052,  11.993,  0.32426, 1.4309),
    'fan_in_16':    (4.7610,  2.6683,  0.17444, 0.85477),
    'fan_in_64':    (10.722,  6.4710,  0.25857, 0.93848),
    'fan_in_256':   (38.079,  31.599,  2.5496,  3.0954),
    'accumulate':   (2.0837,  1.4268,  0.32631, 1.4033),
    'sparse':       (2.5036,  2.1139,  0.31063, 0.75134),
    'sparse_wide':  (3.0716,  2.0145,  0.35515, 0.89566),
}

# Each bar is a whole fixed-cycle run, so the absolute panel divides by the
# cycle count the workload was driven for (see tiers.rs).
CYCLES = {name: 20_000 if name == 'accumulate' else 10_000 for name in DATA}

# Nodes per workload, for the subtitle's per-node reading.
NODES = {
    'dense_chain': 37, 'fanout': 103, 'fan_in_16': 20, 'fan_in_64': 68,
    'fan_in_256': 260, 'accumulate': 3, 'sparse': 205, 'sparse_wide': 781,
}

# Legacy is the engine being replaced, not a wingfoil tier, so it wears the
# neutral rather than a slot in the blue ramp.
SERIES = [
    ('legacy engine', p.NEUTRAL_DARK),
    ('interpreted', p.INTERPRETED),
    ('compiled island', p.ISLAND),
    ('compiled', p.COMPILED),
]
# DATA's tuple order is (legacy, interpreted, compiled, nested); SERIES reads
# legacy, interpreted, island, compiled — so absolute rows are picked by index.
COLUMN = {'legacy engine': 0, 'interpreted': 1, 'compiled island': 3, 'compiled': 2}

names = list(DATA)
y = np.arange(len(names))
ns_per_cycle = {
    label: np.array([DATA[n][COLUMN[label]] * 1e6 / CYCLES[n] for n in names])
    for label, _ in SERIES
}
ratios = np.array([DATA[n][1] / DATA[n][0] for n in names])

fig, (ax_abs, ax_rel) = plt.subplots(
    1, 2, figsize=(13, 5.6), facecolor=p.SURFACE, sharey=True,
    gridspec_kw={'width_ratios': [1.25, 1]},
)

# --- left: what one cycle costs ---------------------------------------------
# A dumbbell: all four engines on one row, joined by a connector, so the length
# of the connector *is* what compilation buys on that workload. Marks carry a
# surface-coloured ring so a near-tie (interpreted and the island on
# `accumulate`) still reads as two marks.
ax_abs.hlines(y, ns_per_cycle['compiled'], ns_per_cycle['legacy engine'],
              color=p.GRID, linewidth=1.6, zorder=1)
for label, colour in SERIES:
    ax_abs.plot(ns_per_cycle[label], y, 'o', markersize=9, color=colour,
                label=label, markeredgecolor=p.SURFACE, markeredgewidth=1.8,
                zorder=4, linestyle='none')

ax_abs.set_xscale('log')
ax_abs.set_xlim(9, 6_000)
ax_abs.xaxis.set_major_formatter(ticker.FuncFormatter(p.fmt_time))
ax_abs.xaxis.set_minor_formatter(ticker.NullFormatter())
p.style(ax_abs, xlabel='Time for one engine cycle through the whole graph — log scale',
        title='One graph, four engines',
        subtitle='Compiling the same wiring is worth 4.4×–37×')
ax_abs.grid(axis='y', visible=False)
ax_abs.legend(fontsize=9.5, loc='upper center', bbox_to_anchor=(0.5, -0.13),
              ncol=4, frameon=False, labelcolor=p.INK_SOFT, handletextpad=0.3,
              columnspacing=1.6)

# --- right: the parity gate --------------------------------------------------
ax_rel.barh(y, ratios, height=0.46, color=p.INTERPRETED, linewidth=0, zorder=2)
for i, value in enumerate(ratios):
    ax_rel.text(value + 0.015, i, f'{value:.2f}×', va='center', ha='left',
                fontsize=9, color=p.INK_SOFT, zorder=4)

ax_rel.axvline(1.0, color=p.INK, linewidth=1.2, zorder=3)
ax_rel.text(1.0, -0.62, 'legacy = 1.0', fontsize=9, color=p.INK,
            va='center', ha='center')
ax_rel.set_xlim(0, 1.18)
ax_rel.xaxis.set_major_formatter(ticker.FuncFormatter(lambda v, _: f'{v:g}×'))
p.style(ax_rel, xlabel='Interpreted tier, relative to the legacy engine — lower is faster',
        title='Faster than the engine it replaces, on all eight',
        subtitle='The parity gate the port had to clear before the tiers mattered')
ax_rel.grid(axis='y', visible=False)

ax_abs.set_yticks(y)
ax_abs.set_yticklabels([f'{n}\n{NODES[n]} nodes' for n in names], fontsize=9.5,
                       color=p.INK)
ax_abs.set_ylim(len(names) - 0.4, -0.75)
ax_abs.tick_params(axis='y', length=0)

fig.tight_layout()
fig.savefig('images/tiers/summary.png', dpi=150, bbox_inches='tight',
            facecolor=p.SURFACE)
plt.close(fig)
print('saved images/tiers/summary.png')

# Renders images/tiers/summary.png — every `tiers` workload's three next
# engines expressed as a ratio against the legacy bar on the same workload.
#
# The suite's headline question is a *relationship*, not a wall-clock number
# ("is next-interpreted at least as fast as legacy, on every workload?"), and
# criterion's own violin plots only ever compare within one group. This is the
# cross-workload view.
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

# Categorical slots 1-3 of the validated default palette, assigned in fixed
# order. Legacy is the 1.0 reference line rather than a fourth bar.
SERIES = [
    ('interpreted', '#2a78d6'),
    ('compiled',    '#eb6834'),
    ('nested',      '#1baf7a'),
]

SURFACE = '#fcfcfb'
INK = '#0b0b0b'
INK_MUTED = '#52514e'

names = list(DATA)
ratios = np.array([[DATA[n][i + 1] / DATA[n][0] for n, in zip(names)]
                   for i in range(3)])

fig, ax = plt.subplots(figsize=(9, 6), facecolor=SURFACE)
ax.set_facecolor(SURFACE)

y = np.arange(len(names))
height = 0.26

# A dot plot, not bars: the axis is logarithmic, so there is no zero for a bar
# length to start from. Each mark sits at its ratio, joined to the legacy
# baseline by a thin connector, so "which side of 1.0" reads at a glance.
for i, (label, colour) in enumerate(SERIES):
    row = y + (1 - i) * height
    ax.hlines(row, 1.0, ratios[i], color=colour, linewidth=2, alpha=0.55,
              zorder=2)
    ax.plot(ratios[i], row, 'o', markersize=9, color=colour, label=label,
            markeredgecolor=SURFACE, markeredgewidth=2, zorder=4,
            linestyle='none')
    for value, yy in zip(ratios[i], row):
        # Label on the far side of the mark from the baseline, so nothing
        # collides with the reference line.
        left = value < 1.0
        ax.text(value * (0.92 if left else 1.09), yy, f'{value:.2f}×',
                va='center', ha='right' if left else 'left',
                fontsize=8.5, color=INK_MUTED, zorder=5)

ax.axvline(1.0, color=INK, linewidth=1.2, zorder=3)
ax.text(1.0, -0.75, 'legacy = 1.0', fontsize=9, color=INK,
        va='center', ha='center')

ax.set_xscale('log')
ax.set_xlim(0.022, 3.6)
ax.xaxis.set_major_locator(ticker.FixedLocator([0.03, 0.1, 0.3, 1.0, 3.0]))
ax.xaxis.set_major_formatter(ticker.FuncFormatter(lambda v, _: f'{v:g}×'))
ax.xaxis.set_minor_formatter(ticker.NullFormatter())

ax.set_yticks(y)
ax.set_yticklabels(names, fontsize=10, color=INK)
ax.set_ylim(len(names) - 0.45, -1.15)

ax.grid(True, axis='x', which='major', linestyle='-', linewidth=0.7, alpha=0.35)
ax.set_axisbelow(True)
for side in ('top', 'right', 'left'):
    ax.spines[side].set_visible(False)
ax.spines['bottom'].set_color('#cfcec9')
ax.tick_params(colors=INK_MUTED, length=0)

ax.set_xlabel('Run time relative to the legacy engine — lower is faster',
              fontsize=11, color=INK_MUTED)
ax.set_title('nitro! execution tiers vs the legacy engine, per workload',
             fontsize=13, fontweight='bold', color=INK, loc='left')
ax.legend(fontsize=10, loc='upper center', bbox_to_anchor=(0.5, -0.11),
          ncol=3, frameon=False, labelcolor=INK, handletextpad=0.4,
          columnspacing=2.0)

fig.tight_layout()
fig.savefig('images/tiers/summary.png', dpi=150, bbox_inches='tight',
            facecolor=SURFACE)
print('saved images/tiers/summary.png')

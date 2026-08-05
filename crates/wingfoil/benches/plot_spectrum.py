# Renders images/spectrum.png — where wingfoil sits on the latency ladder, with
# the engine's own measured costs plotted underneath the budgets of each class.
#
# The point of the chart is the *distance* between the two blocks: every
# wingfoil number measured in this report sits one to three decades below the
# budget of the class the engine serves, and the classes it does not serve are
# lost on transports and toolchains rather than on engine overhead. The upper
# block is industry shape, not measurement — only the lower block comes from
# this suite, and it is labelled that way on the chart because the two must not
# be read as the same kind of claim.
#
#   python plot_spectrum.py
#
# Marker values are *readings* from README.md; refill them when that page is
# re-captured. Nanoseconds.
import matplotlib

matplotlib.use('Agg')
import matplotlib.pyplot as plt
import matplotlib.ticker as ticker

import palette as p

# (label, low, high, note, is_wingfoil) — wire-to-wire / tick-to-trade budgets.
BANDS = [
    ('FPGA wire-to-wire', 50, 1_000, 'no software framework competes', False),
    ('Software HFT, kernel bypass', 1_000, 10_000,
     'engine core is credible; ingress and ops discipline are not', False),
    ('Mid-tier latency systems', 10_000, 1_000_000,
     'what wingfoil serves today — and backtests deterministically', True),
    ('GC / interpreted stacks', 1_000_000, 50_000_000,
     'one collector pause is the whole budget', False),
]

# (label, low, high) — every one measured in this report. high is None for a
# point estimate; the shared-memory hop is quoted as a range, so it stays one.
MARKS = [
    ('Compiled cycle, 37-node graph', 19, None),
    ('Reading the graph clock', 24, None),
    ('Interpreted cycle, 13-node graph', 287, None),
    ('Pooled channel ingress, per message', 870, None),
    ('Shared-memory hop, process to process', 1_000, 5_000),
]

ROW_H = 0.62
GAP = 1.5  # blank rows between the two blocks, room for their headings

# Lay the chart out top-down: budgets first, then the measurement block.
band_ys = [len(MARKS) + GAP + (len(BANDS) - 1 - i) for i in range(len(BANDS))]
mark_ys = [len(MARKS) - 1 - i for i in range(len(MARKS))]

fig, ax = plt.subplots(figsize=(11, 5.6))

for (label, low, high, note, mine), y in zip(BANDS, band_ys):
    color = p.WINGFOIL if mine else p.NEUTRAL
    ax.barh(y, high - low, left=low, height=ROW_H, color=color,
            alpha=1.0 if mine else 0.5, linewidth=0, zorder=2)
    ax.text(low, y + ROW_H / 2 + 0.12, note, ha='left', va='bottom', fontsize=8.6,
            color=p.INK_SOFT if mine else p.INK_MUTED, style='italic')
    ax.text(high * 1.25, y, f'{p.fmt_time(low)} – {p.fmt_time(high)}', ha='left',
            va='center', fontsize=9, color=p.INK_MUTED)

for (label, low, high), y in zip(MARKS, mark_ys):
    if high is None:
        ax.scatter([low], [y], s=70, color=p.COMPILED, zorder=3, linewidths=0)
        ax.text(low * 1.35, y, p.fmt_time(low), ha='left', va='center',
                fontsize=9.5, color=p.INK)
    else:
        ax.barh(y, high - low, left=low, height=ROW_H * 0.45, color=p.COMPILED,
                linewidth=0, zorder=3)
        ax.text(high * 1.35, y, f'{p.fmt_time(low)} – {p.fmt_time(high)}',
                ha='left', va='center', fontsize=9.5, color=p.INK)

divider = len(MARKS) + GAP / 2 - 0.25
ax.axhline(divider, color=p.GRID, linewidth=1)
ax.text(9.5, band_ys[0] + 0.95, 'BUDGET BY CLASS — industry shape, not measured here',
        ha='left', va='bottom', fontsize=8.6, color=p.INK_MUTED, fontweight='bold')
ax.text(9.5, divider - 0.25, 'MEASURED IN THIS REPORT', ha='left', va='top',
        fontsize=8.6, color=p.INK_MUTED, fontweight='bold')

ax.set_yticks(band_ys + mark_ys)
ax.set_yticklabels(
    [b[0] for b in BANDS] + [m[0] for m in MARKS], fontsize=10, color=p.INK
)
for tick, mine in zip(ax.get_yticklabels(), [b[4] for b in BANDS] + [False] * len(MARKS)):
    if mine:
        tick.set_fontweight('bold')

ax.set_xscale('log')
ax.set_xlim(9, 300_000_000)
ax.set_ylim(-0.75, band_ys[0] + 1.75)
ax.xaxis.set_major_locator(ticker.LogLocator(base=10))
ax.xaxis.set_major_formatter(ticker.FuncFormatter(p.fmt_time))
ax.xaxis.set_minor_formatter(ticker.NullFormatter())
p.style(ax, xlabel='Latency (log scale)', title='Where wingfoil sits')
ax.grid(axis='y', visible=False)
ax.spines['left'].set_visible(False)
ax.tick_params(axis='y', length=0)

fig.tight_layout()
fig.savefig('images/spectrum.png', dpi=150, bbox_inches='tight', facecolor=p.SURFACE)
plt.close(fig)
print('saved images/spectrum.png')

"""Shared colour language for the benchmark charts.

One page, one encoding, so meaning carries between charts:

    blue    wingfoil — and *which* blue says which tier: light interpreted,
            mid compiled island, dark whole-program compiled
    orange  rxrust (per-path reactive baseline)
    aqua    tokio async streams (per-path baseline)
    grey    everything that is not wingfoil engine work — the bench harness
            floor, the legacy reference line, latency classes we do not serve

The hexes are light-mode steps of the validated palette the repo's charts
already use: categorical slots 1–3 for the three *families*, and the blue
sequential ramp (steps 300 / 450 / 650) for the tier ordering inside the
wingfoil family. The family hues clear the all-pairs CVD and normal-vision
floors; the tier ramp is monotone in lightness with its light end clear of the
surface. Every series is direct-labelled as well as coloured, so identity never
rests on hue alone — which is also what the aqua slot's sub-3:1 contrast
against a white surface requires.

Charts render on a white surface because they are PNGs embedded in a README:
GitHub shows the same file to light- and dark-theme readers, so there is no
theme to select against.
"""

# --- families ---------------------------------------------------------------
WINGFOIL = '#2a78d6'   # categorical slot 1 — the family colour
RXRUST = '#eb6834'     # categorical slot 2
TOKIO = '#1baf7a'      # categorical slot 3

# --- tiers, as an ordinal ramp inside the wingfoil family --------------------
# Light -> dark reads as less -> more compilation, which is also fast -> faster.
INTERPRETED = '#6da7ec'  # blue 300
ISLAND = '#2a78d6'       # blue 450 (the family colour itself)
COMPILED = '#104281'     # blue 650
BAND = '#cde2fb'         # blue 100 — fill between the fastest and slowest tier

# --- neutrals ---------------------------------------------------------------
INK = '#0b0b0b'
INK_SOFT = '#52514e'
INK_MUTED = '#77766f'
GRID = '#d8d7d2'
NEUTRAL = '#9c9b94'      # harness floor, legacy reference, "not us" bands
NEUTRAL_DARK = '#6f6e68'
SURFACE = '#ffffff'

TIERS = (
    ('interpreted', INTERPRETED),
    ('compiled island', ISLAND),
    ('compiled', COMPILED),
)


def fmt_time(value, _pos=None):
    """Axis formatter: nanoseconds below 1 µs, then µs, then ms.

    Keeps one decimal below 10 of a unit, so a µs axis stepping in halves does
    not print three consecutive ticks reading `2 µs`.
    """
    for scale, unit in ((1_000_000, 'ms'), (1_000, 'µs'), (1, 'ns')):
        if value >= scale or scale == 1:
            scaled = value / scale
            digits = 1 if 0 < scaled < 10 and scaled != int(scaled) else 0
            return f'{scaled:.{digits}f} {unit}'


def style(ax, xlabel=None, ylabel=None, title=None, subtitle=None):
    """Recessive grid and axes, ink-coloured text, no chartjunk."""
    ax.grid(True, which='major', linestyle='-', linewidth=0.8, color=GRID, alpha=0.9)
    ax.grid(True, which='minor', linestyle='--', linewidth=0.5, color=GRID, alpha=0.6)
    ax.set_axisbelow(True)
    for side in ('top', 'right'):
        ax.spines[side].set_visible(False)
    for side in ('left', 'bottom'):
        ax.spines[side].set_color(GRID)
    ax.tick_params(colors=INK_SOFT, labelsize=10)
    if xlabel:
        ax.set_xlabel(xlabel, fontsize=11, color=INK_SOFT)
    if ylabel:
        ax.set_ylabel(ylabel, fontsize=11, color=INK_SOFT)
    if title:
        ax.set_title(title, fontsize=13, fontweight='bold', color=INK, loc='left',
                     pad=18 if subtitle else 10)
    if subtitle:
        ax.text(0, 1.02, subtitle, transform=ax.transAxes, fontsize=10,
                color=INK_MUTED, va='bottom', ha='left')

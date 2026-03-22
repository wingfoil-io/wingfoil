import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
import matplotlib.ticker as ticker

depths = list(range(1, 11))

wingfoil = [174, 212, 197, 256, 264, 267, 287, 326, 301, 352]
async_s  = [109, 165, 274, 545, 1042, 2045, 4076, 8100, 16125, 32121]
reactive = [66,  156, 324, 652, 1349, 2679, 5353, 10732, 22493, 43073]

fig, ax = plt.subplots(figsize=(8, 5))

ax.plot(depths, wingfoil, 'o-', color='#2196F3', linewidth=2, markersize=6, label='wingfoil (BFS)')
ax.plot(depths, async_s,  's-', color='#FF9800', linewidth=2, markersize=6, label='async streams (DFS)')
ax.plot(depths, reactive, '^-', color='#F44336', linewidth=2, markersize=6, label='reactive / rxrust (DFS)')

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
ax.set_title('BFS vs DFS: branch/recombine latency', fontsize=13, fontweight='bold')
ax.legend(fontsize=11)

fig.tight_layout()
fig.savefig('latency.png', dpi=150, bbox_inches='tight')
print("saved")

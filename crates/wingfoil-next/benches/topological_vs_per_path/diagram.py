import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt

DEPTH = 2
NODE_R = 0.28
SPACING = 1.1

positions = [(0, (DEPTH - i) * SPACING) for i in range(DEPTH + 1)]
node_labels = ['src'] + [f'add{chr(0x2080 + i+1)}' for i in range(DEPTH)]
node_colors = ['#4CAF50'] + ['#2196F3'] * DEPTH

fig, ax = plt.subplots(figsize=(2.5, 4))

# Edges
for i in range(DEPTH):
    x0, y0 = positions[i]
    x1, y1 = positions[i + 1]
    for rad in (-0.35, 0.35):
        ax.annotate(
            '', xy=(x1, y1 + NODE_R), xytext=(x0, y0 - NODE_R),
            arrowprops=dict(arrowstyle='->', color='#555', lw=1.8,
                            connectionstyle=f'arc3,rad={rad}', mutation_scale=16),
            zorder=1,
        )

# Nodes
for (x, y), label, color in zip(positions, node_labels, node_colors):
    ax.add_patch(plt.Circle((x, y), NODE_R, color=color, zorder=2))
    ax.text(x, y, label, ha='center', va='center', color='white',
            fontsize=10, fontweight='bold', zorder=3)

# × 2 annotations
for i in range(DEPTH):
    mid_y = (positions[i][1] + positions[i+1][1]) / 2
    ax.text(0.55, mid_y, '× 2', ha='left', va='center',
            fontsize=9, color='#888', style='italic')

# Dashed continuation
last_x, last_y = positions[-1]
ax.annotate(
    '', xy=(last_x, last_y - SPACING * 0.5), xytext=(last_x, last_y - NODE_R),
    arrowprops=dict(arrowstyle='->', color='#888', lw=1.5, linestyle='dashed'),
    zorder=1,
)
ax.text(last_x, last_y - SPACING * 0.62, '⋮', ha='center', va='center',
        fontsize=16, color='#555')
ax.text(last_x, last_y - SPACING * 0.85, 'N levels', ha='center', va='center',
        fontsize=9, color='#888', style='italic')

last_y_plot = last_y - SPACING * 0.95
ax.set_xlim(-0.9, 1.0)
ax.set_ylim(last_y_plot, DEPTH * SPACING + NODE_R + 0.1)
ax.set_aspect('equal')
ax.axis('off')

fig.savefig('diagram.png', dpi=150, bbox_inches='tight', pad_inches=0.05)
print("saved")

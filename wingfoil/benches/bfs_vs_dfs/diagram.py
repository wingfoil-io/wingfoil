import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt

DEPTH = 4
NODE_R = 0.28
SPACING = 1.6

fig, ax = plt.subplots(figsize=(4, 7))

positions = [(0, (DEPTH - i) * SPACING) for i in range(DEPTH + 1)]

node_labels = ['src'] + [f'add{chr(0x2080 + i+1)}' for i in range(DEPTH)]
node_colors = ['#4CAF50'] + ['#2196F3'] * DEPTH

# Draw edges first (behind nodes)
for i in range(DEPTH):
    x0, y0 = positions[i]
    x1, y1 = positions[i + 1]
    for rad in (-0.35, 0.35):
        ax.annotate(
            '', xy=(x1, y1 + NODE_R), xytext=(x0, y0 - NODE_R),
            arrowprops=dict(
                arrowstyle='->', color='#555', lw=1.8,
                connectionstyle=f'arc3,rad={rad}',
                mutation_scale=16,
            ),
            zorder=1,
        )

# Draw nodes
for (x, y), label, color in zip(positions, node_labels, node_colors):
    circle = plt.Circle((x, y), NODE_R, color=color, zorder=2, linewidth=1.5,
                         edgecolor='white')
    ax.add_patch(circle)
    ax.text(x, y, label, ha='center', va='center', color='white',
            fontsize=10, fontweight='bold', zorder=3)

# Annotations on the right
for i in range(DEPTH):
    x0, y0 = positions[i]
    x1, y1 = positions[i + 1]
    mid_y = (y0 + y1) / 2
    ax.text(0.7, mid_y, '× 2', ha='left', va='center',
            fontsize=9, color='#888', style='italic')

ax.set_xlim(-1.2, 1.4)
ax.set_ylim(-0.6, DEPTH * SPACING + 0.6)
ax.set_aspect('equal')
ax.axis('off')
ax.set_title('Branch / recombine pattern\n(both inputs from the same upstream node)',
             fontsize=10, pad=12)

fig.tight_layout()
fig.savefig('diagram.png', dpi=150, bbox_inches='tight')
print("saved")

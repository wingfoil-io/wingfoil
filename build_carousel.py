#!/usr/bin/env python3
"""
Wingfoil LinkedIn carousel generator.

Produces wingfoil-carousel.pdf — an 11-slide, 1080x1350 portrait "document post"
to drive developer engagement, contributors and GitHub stars for wingfoil, an
open-source ultra-low-latency stream-processing framework in Rust.

Positioning (locked):
  Headline: "The fastest way to build a fast trading stack."
  ("fastest" and "fast" render in the brand gradient; the rest is white.)

Render-safe gradient: every gradient word is drawn glyph-by-glyph with a SOLID
fill colour interpolated along the magenta -> violet -> cyan stops. We never use
text-clip + linearGradient (mode 7), which drops out black-on-black in some
mobile PDF viewers.

Run:  python3 build_carousel.py
"""

from reportlab.pdfgen import canvas
from reportlab.pdfbase.pdfmetrics import stringWidth

# ----------------------------------------------------------------------------
# Page geometry
# ----------------------------------------------------------------------------
W, H = 1080, 1350


def yt(top):
    """Convert a top-based y (0 = top of page) to reportlab's bottom origin."""
    return H - top


# ----------------------------------------------------------------------------
# Palette (sampled from the wingfoil.io homepage)
# ----------------------------------------------------------------------------
def hx(h):
    h = h.lstrip("#")
    return tuple(int(h[i : i + 2], 16) / 255 for i in (0, 2, 4))


BG        = hx("000000")
GRAD_A    = hx("FF30C8")  # magenta
GRAD_B    = hx("9B49E8")  # violet
GRAD_C    = hx("2999FF")  # cyan
CTA_BLUE  = hx("2672FB")
WORDMARK  = hx("DFE5EB")
HEADING   = hx("FBFBFB")
BODY      = hx("C2C8CF")
MUTED     = hx("7E868F")
PANEL_BG  = hx("0C0D11")
PANEL_BD  = hx("23262D")

# Syntax-highlight roles for code panels
SYN_KW   = CTA_BLUE        # keywords
SYN_FN   = GRAD_B          # function names
SYN_TY   = GRAD_C          # types
SYN_STR  = GRAD_A          # string literals
SYN_COM  = MUTED           # comments
SYN_TXT  = BODY            # everything else

GRAD_STOPS = [GRAD_A, GRAD_B, GRAD_C]

FONT      = "Helvetica"
FONT_BOLD = "Helvetica-Bold"
FONT_MONO = "Courier"
FONT_MONOB = "Courier-Bold"


# ----------------------------------------------------------------------------
# Colour helpers
# ----------------------------------------------------------------------------
def grad_color(t):
    """Interpolate the 3-stop brand gradient at position t in [0, 1]."""
    t = max(0.0, min(1.0, t))
    n = len(GRAD_STOPS) - 1
    seg = t * n
    i = min(int(seg), n - 1)
    f = seg - i
    a, b = GRAD_STOPS[i], GRAD_STOPS[i + 1]
    return tuple(a[k] + (b[k] - a[k]) * f for k in range(3))


def set_fill(c, rgb):
    c.setFillColorRGB(*rgb)


def set_stroke(c, rgb):
    c.setStrokeColorRGB(*rgb)


# ----------------------------------------------------------------------------
# Text helpers (top-based positioning)
# ----------------------------------------------------------------------------
def text(c, x, top, s, font, size, color, anchor="left", tracking=0.0):
    """Draw a solid-colour string anchored by its top edge."""
    baseline = yt(top) - size * 0.74
    w = string_width_tracked(s, font, size, tracking)
    if anchor == "center":
        x -= w / 2
    elif anchor == "right":
        x -= w
    set_fill(c, color)
    if tracking:
        cx = x
        for ch in s:
            c.setFont(font, size)
            c.drawString(cx, baseline, ch)
            cx += stringWidth(ch, font, size) + tracking
    else:
        c.setFont(font, size)
        c.drawString(x, baseline, s)
    return w


def string_width_tracked(s, font, size, tracking=0.0):
    w = stringWidth(s, font, size)
    if tracking and len(s) > 1:
        w += tracking * (len(s) - 1)
    return w


def grad_word(c, x, baseline, word, font, size):
    """Draw a word glyph-by-glyph with solid per-glyph gradient fills."""
    n = max(len(word) - 1, 1)
    cx = x
    for i, ch in enumerate(word):
        set_fill(c, grad_color(i / n))
        c.setFont(font, size)
        c.drawString(cx, baseline, ch)
        cx += stringWidth(ch, font, size)
    return cx - x


def rich_line(c, segments, top, font, size, x_center=None, x_left=None, leading_space=True):
    """
    Render a line built from segments. Each segment is (text, kind) where kind is
    "white", "head", "muted", "body", or "grad". Gradient segments are rendered
    with per-glyph interpolated solid fills. Returns total width.
    """
    # measure
    total = sum(stringWidth(s, font, size) for s, _ in segments)
    if x_center is not None:
        x = x_center - total / 2
    else:
        x = x_left
    baseline = yt(top) - size * 0.74
    color_map = {
        "white": HEADING,
        "head": HEADING,
        "muted": MUTED,
        "body": BODY,
        "cta": CTA_BLUE,
    }
    for s, kind in segments:
        if kind == "grad":
            x += grad_word(c, x, baseline, s, font, size)
        else:
            set_fill(c, color_map.get(kind, HEADING))
            c.setFont(font, size)
            c.drawString(x, baseline, s)
            x += stringWidth(s, font, size)
    return total


# ----------------------------------------------------------------------------
# Shared furniture
# ----------------------------------------------------------------------------
MARGIN = 84


def background(c):
    set_fill(c, BG)
    c.rect(0, 0, W, H, stroke=0, fill=1)


def gradient_bar(c, x, top, w, h):
    """A thin horizontal accent bar, drawn as gradient slices."""
    steps = 120
    sw = w / steps
    y = yt(top) - h
    for i in range(steps):
        set_fill(c, grad_color(i / (steps - 1)))
        c.rect(x + i * sw, y, sw + 0.6, h, stroke=0, fill=1)


def footer(c, idx):
    """Wordmark left, slide counter right."""
    text(c, MARGIN, H - 64, "wingfoil", FONT_BOLD, 26, WORDMARK)
    text(c, W - MARGIN, H - 64, f"{idx:02d} / 11", FONT, 22, MUTED, anchor="right")


def kicker(c, top, s):
    gradient_bar(c, MARGIN, top, 54, 6)
    text(c, MARGIN + 70, top - 4, s, FONT_BOLD, 22, MUTED, tracking=3.2)


def rounded_panel(c, x, top, w, h, bg=PANEL_BG, bd=PANEL_BD, r=22, lw=1.5):
    set_fill(c, bg)
    set_stroke(c, bd)
    c.setLineWidth(lw)
    c.roundRect(x, yt(top) - h, w, h, r, stroke=1, fill=1)


# ----------------------------------------------------------------------------
# Code panel rendering
# ----------------------------------------------------------------------------
def code_panel(c, top, lines, x=MARGIN, w=W - 2 * MARGIN, size=27, leading=42, pad=40):
    """
    Render a syntax-highlighted code panel.
    `lines` is a list of rows; each row is a list of (text, role) spans.
    Roles: 'kw','fn','ty','str','com','txt'.
    """
    role_color = {
        "kw": SYN_KW, "fn": SYN_FN, "ty": SYN_TY,
        "str": SYN_STR, "com": SYN_COM, "txt": SYN_TXT,
    }
    h = pad * 2 + leading * len(lines)
    rounded_panel(c, x, top, w, h)
    # window dots
    for i, col in enumerate([GRAD_A, GRAD_B, GRAD_C]):
        set_fill(c, col)
        c.circle(x + pad + i * 26, yt(top) - 30, 7, stroke=0, fill=1)
    y0 = top + pad + 34
    for r, row in enumerate(lines):
        baseline = yt(y0 + r * leading) - size * 0.74
        cx = x + pad
        for s, role in row:
            set_fill(c, role_color.get(role, SYN_TXT))
            c.setFont(FONT_MONO, size)
            c.drawString(cx, baseline, s)
            cx += stringWidth(s, FONT_MONO, size)
    return h


# ----------------------------------------------------------------------------
# Slides
# ----------------------------------------------------------------------------
def slide_01(c):
    """Hook — locked headline."""
    background(c)
    kicker(c, 150, "ELECTRONIC TRADING  ·  RUST")

    # Headline, gradient on "fastest" and "fast".
    size = 92
    lh = 104
    top = 360
    cx = W / 2
    lines = [
        [("The ", "white"), ("fastest", "grad"), (" way to", "white")],
        [("build a ", "white"), ("fast", "grad"), (" trading", "white")],
        [("stack.", "white")],
    ]
    for i, segs in enumerate(lines):
        rich_line(c, segs, top + i * lh, FONT_BOLD, size, x_center=cx)

    # Subhead
    sub = ("Market data, messaging, storage, backtesting, execution —")
    sub2 = ("one ultra-low-latency Rust graph.")
    text(c, cx, 700, sub, FONT, 32, BODY, anchor="center")
    text(c, cx, 744, sub2, FONT, 32, BODY, anchor="center")

    # Bottom tag pill
    tag = "BATTERIES INCLUDED  ·  OPEN SOURCE"
    tw = string_width_tracked(tag, FONT_BOLD, 22, 3.0)
    px = cx - (tw + 64) / 2
    rounded_panel(c, px, 1120, tw + 64, 66, bg=PANEL_BG, bd=PANEL_BD, r=33)
    text(c, cx, 1140, tag, FONT_BOLD, 22, BODY, anchor="center", tracking=3.0)

    gradient_bar(c, cx - 130, 880, 260, 8)
    footer(c, 1)


def slide_02(c):
    """The problem."""
    background(c)
    kicker(c, 150, "THE PROBLEM")
    text(c, MARGIN, 230, "Every backend speaks a", FONT_BOLD, 60, HEADING)
    rich_line(c, [("different ", "white"), ("language", "grad"), (".", "white")],
              306, FONT_BOLD, 60, x_left=MARGIN)

    intro = ("Sockets. Brokers. Databases. Each with its own client, its own "
             "threading model, its own idea of time.")
    wrap_text(c, intro, MARGIN, 430, W - 2 * MARGIN, FONT, 31, BODY, 44)

    rows = [
        ("Market data feeds", "UDP multicast, shared memory, vendor SDKs"),
        ("Message buses", "Kafka, Fluvio, ZeroMQ — each a bespoke consumer loop"),
        ("Databases", "request/response, batch, time-series quirks"),
        ("Execution venues", "FIX sessions, sequence numbers, heartbeats"),
    ]
    top = 580
    rh = 132
    for i, (h, d) in enumerate(rows):
        rounded_panel(c, MARGIN, top + i * rh, W - 2 * MARGIN, rh - 22)
        text(c, MARGIN + 34, top + i * rh + 26, h, FONT_BOLD, 30, HEADING)
        text(c, MARGIN + 34, top + i * rh + 68, d, FONT, 25, MUTED)

    text(c, MARGIN, 1170, "Wiring them into one low-latency graph leaks",
         FONT, 27, BODY)
    rich_line(c, [("boilerplate ", "grad"), ("— and ", "body"), ("latency", "grad"),
                  (".", "body")], 1210, FONT, 27, x_left=MARGIN)
    footer(c, 2)


def slide_03(c):
    """What wingfoil is."""
    background(c)
    kicker(c, 150, "WHAT WINGFOIL IS")
    text(c, MARGIN, 230, "A graph engine for", FONT_BOLD, 60, HEADING)
    rich_line(c, [("real-time ", "grad"), ("Rust.", "white")],
              306, FONT_BOLD, 60, x_left=MARGIN)

    intro = ("You declare a directed graph (DAG) of transformations. wingfoil "
             "runs it — breadth-first, in dependency order, on every tick.")
    wrap_text(c, intro, MARGIN, 430, W - 2 * MARGIN, FONT, 31, BODY, 44)

    cards = [
        ("DAG engine", "Nodes declare upstreams; the engine schedules them. "
                        "~20 ns of overhead per node cycle."),
        ("Adapters at the edges", "I/O lives at the boundary of the graph. The "
                                  "core stays pure data-flow."),
        ("Async + multi-threaded", "Tokio-backed async sources and sinks, plus "
                                    "dedicated OS threads where latency demands it."),
    ]
    top = 600
    ch = 188
    for i, (h, d) in enumerate(cards):
        rounded_panel(c, MARGIN, top + i * ch, W - 2 * MARGIN, ch - 26)
        gradient_bar(c, MARGIN + 34, top + i * ch + 36, 44, 6)
        text(c, MARGIN + 34, top + i * ch + 58, h, FONT_BOLD, 34, HEADING)
        wrap_text(c, d, MARGIN + 34, top + i * ch + 106, W - 2 * MARGIN - 68,
                  FONT, 26, BODY, 36)
    footer(c, 3)


def slide_04(c):
    """Mental model — sources -> [graph] -> sinks."""
    background(c)
    kicker(c, 150, "MENTAL MODEL")
    text(c, MARGIN, 230, "Adapters are the edges", FONT_BOLD, 56, HEADING)
    rich_line(c, [("of the ", "white"), ("graph", "grad"), (".", "white")],
              300, FONT_BOLD, 56, x_left=MARGIN)

    # Layout: SOURCES column -> GRAPH node -> SINKS column
    box_w = 240
    box_h = 92
    row_top0 = 540
    row_step = 150

    # graph node (gradient border), vertically centred against the three rows
    g_w, g_h = 220, 220
    g_x = W / 2 - g_w / 2
    g_top = 600
    g_cy = yt(g_top + g_h / 2)            # reportlab y of graph centre
    g_left = g_x
    g_right = g_x + g_w

    # sources (left) — arrows point INTO the graph
    src_x = MARGIN
    srcs = ["Market data", "Message bus", "Database"]
    for i, s in enumerate(srcs):
        by = row_top0 + i * row_step
        rounded_panel(c, src_x, by, box_w, box_h, bd=GRAD_C)
        text(c, src_x + box_w / 2, by + 32, s, FONT_BOLD, 26, HEADING, anchor="center")
        arrow(c, src_x + box_w + 10, yt(by + box_h / 2), g_left - 10, g_cy)

    grad_border_panel(c, g_x, g_top, g_w, g_h, r=26)
    text(c, g_x + g_w / 2, g_top + 80, "GRAPH", FONT_BOLD, 34, HEADING, anchor="center")
    text(c, g_x + g_w / 2, g_top + 124, "DAG", FONT, 24, MUTED, anchor="center")

    # sinks (right) — arrows point OUT of the graph
    sink_x = W - MARGIN - box_w
    sinks = ["Execution (FIX)", "Telemetry", "Storage"]
    for i, s in enumerate(sinks):
        by = row_top0 + i * row_step
        rounded_panel(c, sink_x, by, box_w, box_h, bd=GRAD_A)
        text(c, sink_x + box_w / 2, by + 32, s, FONT_BOLD, 24, HEADING, anchor="center")
        arrow(c, g_right + 10, g_cy, sink_x - 10, yt(by + box_h / 2))

    text(c, W / 2, 980, "Sources →  [ graph ]  → sinks.",
         FONT_BOLD, 34, HEADING, anchor="center")
    text(c, W / 2, 1035, "Swap an adapter; the graph doesn't change.",
         FONT, 28, BODY, anchor="center")
    footer(c, 4)


def grad_border_panel(c, x, top, w, h, r=22, lw=4):
    """A panel with a gradient border, fill = panel bg."""
    # draw gradient as many thin concentric-ish strokes along the rounded rect path
    set_fill(c, PANEL_BG)
    # gradient ring: draw a slightly larger gradient-filled roundrect, then inner bg
    steps = 100
    sw = w / steps
    y = yt(top) - h
    # top/bottom gradient frame using clipped slices is complex; approximate with
    # a gradient outline by stroking small segments around the perimeter.
    c.saveState()
    p = c.beginPath()
    p.roundRect(x - lw, y - lw, w + 2 * lw, h + 2 * lw, r + lw)
    c.clipPath(p, stroke=0, fill=0)
    for i in range(steps):
        set_fill(c, grad_color(i / (steps - 1)))
        c.rect(x - lw + i * (w + 2 * lw) / steps, y - lw,
               (w + 2 * lw) / steps + 0.6, h + 2 * lw, stroke=0, fill=1)
    c.restoreState()
    set_fill(c, PANEL_BG)
    c.roundRect(x, y, w, h, r, stroke=0, fill=1)


def arrow(c, x1, y1, x2, y2):
    """Cyan arrow from (x1,y1) to (x2,y2) in reportlab coords."""
    set_stroke(c, GRAD_C)
    c.setLineWidth(3)
    c.line(x1, y1, x2, y2)
    # arrowhead
    import math
    ang = math.atan2(y2 - y1, x2 - x1)
    a = 14
    for d in (math.pi - 0.4, math.pi + 0.4):
        c.line(x2, y2, x2 + a * math.cos(ang + d), y2 + a * math.sin(ang + d))


def slide_05(c):
    """The batteries — trading-stack pipeline with a left spine."""
    background(c)
    kicker(c, 150, "THE BATTERIES")
    text(c, MARGIN, 230, "A whole trading stack,", FONT_BOLD, 56, HEADING)
    rich_line(c, [("included", "grad"), (".", "white")],
              300, FONT_BOLD, 56, x_left=MARGIN)

    stages = [
        ("MARKET DATA IN", "Aeron  ·  iceoryx2  ·  ZeroMQ", GRAD_A),
        ("MESSAGING", "Kafka  ·  Fluvio", GRAD_B),
        ("STORAGE + REPLAY", "kdb+   ·   your DB?", GRAD_B),
        ("EXECUTION OUT", "FIX", GRAD_C),
        ("OBSERVABILITY", "OpenTelemetry  ·  Prometheus", GRAD_C),
    ]
    top = 460
    rh = 130
    spine_x = MARGIN + 26
    # left spine
    set_stroke(c, PANEL_BD)
    c.setLineWidth(4)
    c.line(spine_x, yt(top + 30), spine_x, yt(top + (len(stages) - 1) * rh + 50))

    panel_x = MARGIN + 70
    panel_w = W - MARGIN - panel_x
    for i, (label, items, col) in enumerate(stages):
        cy = top + i * rh + 40
        # spine node
        set_fill(c, col)
        c.circle(spine_x, yt(cy), 11, stroke=0, fill=1)
        dashed = "your DB?" in items
        rounded_panel(c, panel_x, top + i * rh, panel_w, rh - 24)
        if dashed:
            # redraw border dashed to tease "your DB?"
            set_stroke(c, GRAD_B)
            c.setLineWidth(1.8)
            c.setDash(6, 6)
            c.roundRect(panel_x, yt(top + i * rh) - (rh - 24), panel_w, rh - 24, 22,
                        stroke=1, fill=0)
            c.setDash()
        text(c, panel_x + 30, top + i * rh + 22, label, FONT_BOLD, 22, MUTED, tracking=2.5)
        text(c, panel_x + 30, top + i * rh + 58, items, FONT_BOLD, 32, HEADING)

    text(c, MARGIN, 1185, "+ CSV · etcd · WebSocket  —  or build your own.",
         FONT, 26, MUTED)
    footer(c, 5)


def spotlight_header(c, idx_label, title, grad_tail):
    kicker(c, 150, idx_label)
    rich_line(c, [(title, "white"), (grad_tail, "grad")],
              224, FONT_BOLD, 56, x_left=MARGIN)


def slide_06(c):
    """Spotlight - IPC (iceoryx2)."""
    background(c)
    spotlight_header(c, "SPOTLIGHT  ·  IPC", "Zero-copy ", "shared memory")
    wrap_text(c, "iceoryx2 hands you bytes straight out of shared memory — no "
                 "serialization, no copy on the hot path.",
              MARGIN, 330, W - 2 * MARGIN, FONT, 29, BODY, 42)

    lines = [
        [("// zero-copy IPC — no serialization on the hot path", "com")],
        [("let", "kw"), (" trades = ", "txt"), ("iceoryx2_sub", "fn"),
         ("::<", "txt"), ("Trade", "ty"), (">(", "txt"), ("\"md/trades\"", "str"),
         (");", "txt")],
        [("", "txt")],
        [("trades", "txt")],
        [("    .", "txt"), ("collapse", "fn"), ("()", "txt")],
        [("    .", "txt"), ("for_each", "fn"), ("(|t, _| ", "txt"),
         ("route", "fn"), ("(t))", "txt")],
        [("    .", "txt"), ("run", "fn"), ("(", "txt"), ("RunMode", "ty"),
         ("::RealTime, ", "txt"), ("RunFor", "ty"), ("::Forever)?;", "txt")],
    ]
    code_panel(c, 470, lines)
    rich_line(c, [("Payloads are ", "body"), ("#[repr(C)]", "grad"),
                  (" — nanoseconds, not microseconds.", "body")],
              1175, FONT, 27, x_left=MARGIN)
    footer(c, 6)


def slide_07(c):
    """Spotlight - Message bus (Kafka)."""
    background(c)
    spotlight_header(c, "SPOTLIGHT  ·  MESSAGE BUS", "One ", "consumer loop")
    wrap_text(c, "Subscribe to a topic and it becomes a graph source. Bursts of "
                 "messages flow downstream like any other stream.",
              MARGIN, 330, W - 2 * MARGIN, FONT, 29, BODY, 42)

    lines = [
        [("let", "kw"), (" conn = ", "txt"), ("KafkaConnection", "ty"),
         ("::", "txt"), ("new", "fn"), ("(", "txt"), ("\"localhost:9092\"", "str"),
         (");", "txt")],
        [("", "txt")],
        [("kafka_sub", "fn"), ("(conn, ", "txt"), ("\"orders\"", "str"),
         (", ", "txt"), ("\"risk-engine\"", "str"), (")", "txt")],
        [("    .", "txt"), ("collapse", "fn"), ("()", "txt")],
        [("    .", "txt"), ("map", "fn"), ("(|ev| ", "txt"), ("decode", "fn"),
         ("(ev.value))", "txt")],
        [("    .", "txt"), ("run", "fn"), ("(", "txt"), ("RunMode", "ty"),
         ("::RealTime, ", "txt"), ("RunFor", "ty"), ("::Forever)?;", "txt")],
    ]
    code_panel(c, 470, lines)
    rich_line(c, [("Same fluent API as ", "body"), ("every", "grad"),
                  (" other source.", "body")],
              1175, FONT, 27, x_left=MARGIN)
    footer(c, 7)


def slide_08(c):
    """Spotlight - Database (kdb)."""
    background(c)
    spotlight_header(c, "SPOTLIGHT  ·  DATABASE", "History as a ", "stream")
    wrap_text(c, "kdb_read pulls history in time-slices via a closure you write. "
                 "Time lives on the graph, not in your records.",
              MARGIN, 330, W - 2 * MARGIN, FONT, 29, BODY, 42)

    lines = [
        [("let", "kw"), (" conn = ", "txt"), ("KdbConnection", "ty"),
         ("::", "txt"), ("new", "fn"), ("(", "txt"), ("\"localhost\"", "str"),
         (", ", "txt"), ("5000", "ty"), (");", "txt")],
        [("", "txt")],
        [("kdb_read", "fn"), ("::<", "txt"), ("Trade", "ty"), (", _>(conn, hour,", "txt")],
        [("    |(t0, t1), date, _| ", "txt"), ("format!", "fn"), ("(", "txt")],
        [("        ", "txt"), ("\"select from trades where \\", "str")],
        [("         ", "txt"), ("date=d+{date}, time>={t0}, time<{t1}\"", "str"), ("))", "txt")],
        [("    .", "txt"), ("collapse", "fn"), ("().", "txt"), ("map", "fn"),
         ("(|t| t.price)", "txt")],
        [("    .", "txt"), ("run", "fn"), ("(", "txt"), ("HistoricalFrom", "ty"),
         ("(start), ", "txt"), ("Duration", "ty"), ("(day))?;", "txt")],
    ]
    code_panel(c, 460, lines, size=25, leading=39)
    rich_line(c, [("Same graph. ", "grad"), ("History or real-time.", "body")],
              1175, FONT, 28, x_left=MARGIN)
    footer(c, 8)


def slide_09(c):
    """Replay = backtesting."""
    background(c)
    kicker(c, 150, "REPLAY = BACKTESTING")
    text(c, MARGIN, 230, "Backtest and production", FONT_BOLD, 54, HEADING)
    rich_line(c, [("are the ", "white"), ("same graph", "grad"), (".", "white")],
              298, FONT_BOLD, 54, x_left=MARGIN)

    wrap_text(c, "Flip one argument. The adapters, the nodes, the wiring — all "
                 "identical. Only the clock changes.",
              MARGIN, 420, W - 2 * MARGIN, FONT, 31, BODY, 44)

    # backtest panel
    text(c, MARGIN, 590, "BACKTEST", FONT_BOLD, 24, GRAD_A, tracking=3)
    lines_bt = [
        [("// replay history through the graph", "com")],
        [("graph.", "txt"), ("run", "fn"), ("(", "txt"), ("RunMode", "ty"),
         ("::", "txt"), ("HistoricalFrom", "ty"), ("(start),", "txt")],
        [("          ", "txt"), ("RunFor", "ty"), ("::", "txt"), ("Duration", "ty"),
         ("(day))?;", "txt")],
    ]
    code_panel(c, 640, lines_bt, size=26, leading=40)

    text(c, MARGIN, 890, "PRODUCTION", FONT_BOLD, 24, GRAD_C, tracking=3)
    lines_pr = [
        [("// same adapters, same graph, wall clock", "com")],
        [("graph.", "txt"), ("run", "fn"), ("(", "txt"), ("RunMode", "ty"),
         ("::", "txt"), ("RealTime", "ty"), (",", "txt")],
        [("          ", "txt"), ("RunFor", "ty"), ("::", "txt"), ("Forever", "ty"),
         (")?;", "txt")],
    ]
    code_panel(c, 940, lines_pr, size=26, leading=40)

    rich_line(c, [("Test on history. ", "body"), ("Deploy unchanged.", "grad")],
              1190, FONT_BOLD, 30, x_left=MARGIN)
    footer(c, 9)


def slide_10(c):
    """Why it's fast — with real benchmark number."""
    background(c)
    kicker(c, 150, "WHY IT'S FAST")
    text(c, MARGIN, 230, "Built for the", FONT_BOLD, 60, HEADING)
    rich_line(c, [("hot path", "grad"), (".", "white")],
              306, FONT_BOLD, 60, x_left=MARGIN)

    # Big benchmark number panel
    rounded_panel(c, MARGIN, 430, W - 2 * MARGIN, 250)
    cx = W / 2
    rich_line(c, [("~20 ns", "grad")], 470, FONT_BOLD, 96, x_center=cx)
    text(c, cx, 590, "engine overhead per node, per cycle", FONT, 30, BODY, anchor="center")
    text(c, cx, 634, "(10×10 DAG, all nodes ticking, 3.8 GHz)", FONT, 22, MUTED, anchor="center")

    cards = [
        ("DAG execution", "Breadth-first, dependency-ordered. Cached upstream "
                          "indices — no per-tick map lookups."),
        ("Zero-copy adapters", "Shared-memory IPC and #[repr(C)] payloads skip "
                               "serialization on the critical path."),
        ("Async + multi-core", "Tokio sources, dedicated OS threads, and spin "
                               "modes where every microsecond counts."),
    ]
    top = 728
    ch = 162
    for i, (h, d) in enumerate(cards):
        rounded_panel(c, MARGIN, top + i * ch, W - 2 * MARGIN, ch - 26)
        gradient_bar(c, MARGIN + 30, top + i * ch + 30, 40, 6)
        text(c, MARGIN + 30, top + i * ch + 50, h, FONT_BOLD, 30, HEADING)
        wrap_text(c, d, MARGIN + 30, top + i * ch + 92, W - 2 * MARGIN - 60,
                  FONT, 24, BODY, 32)
    footer(c, 10)


def slide_11(c):
    """Call to arms."""
    background(c)
    kicker(c, 150, "CALL TO ARMS")
    text(c, MARGIN, 240, "Build your favorite", FONT_BOLD, 62, HEADING)
    rich_line(c, [("DB adapter", "grad"), (".", "white")],
              318, FONT_BOLD, 62, x_left=MARGIN)

    wrap_text(c, "kdb+ is in. Postgres, ClickHouse, DuckDB, Redis aren't — yet. "
                 "There's a skill + template to make it a weekend project.",
              MARGIN, 450, W - 2 * MARGIN, FONT, 31, BODY, 44)

    actions = [
        ("Star the repo", "Two seconds. Helps more devs find it."),
        ("Grab a good-first-issue", "Scoped adapter issues, ready to claim."),
        ("Contributor mentorship", "We pair with you through your first PR."),
    ]
    top = 640
    ch = 138
    for i, (h, d) in enumerate(actions):
        rounded_panel(c, MARGIN, top + i * ch, W - 2 * MARGIN, ch - 24)
        # gradient marker disc with the step number
        mx, my = MARGIN + 56, top + i * ch + (ch - 24) / 2
        set_fill(c, grad_color(i / 2))
        c.circle(mx, yt(my), 20, stroke=0, fill=1)
        text(c, mx, my - 12, str(i + 1), FONT_BOLD, 26, BG, anchor="center")
        text(c, MARGIN + 110, top + i * ch + 28, h, FONT_BOLD, 32, HEADING)
        text(c, MARGIN + 110, top + i * ch + 72, d, FONT, 25, MUTED)

    # CTA button
    bw, bh = W - 2 * MARGIN, 96
    bx = MARGIN
    btop = 1090
    set_fill(c, CTA_BLUE)
    c.roundRect(bx, yt(btop) - bh, bw, bh, 20, stroke=0, fill=1)
    text(c, W / 2, btop + 30, "github.com/wingfoil-io/wingfoil",
         FONT_BOLD, 34, HEADING, anchor="center")
    footer(c, 11)


# ----------------------------------------------------------------------------
# Generic wrap helper
# ----------------------------------------------------------------------------
def wrap_text(c, s, x, top, max_w, font, size, color=BODY, leading=None):
    if leading is None:
        leading = size * 1.4
    words = s.split()
    line = ""
    y = top
    for w in words:
        trial = (line + " " + w).strip()
        if stringWidth(trial, font, size) > max_w and line:
            text(c, x, y, line, font, size, color)
            y += leading
            line = w
        else:
            line = trial
    if line:
        text(c, x, y, line, font, size, color)
    return y


# ----------------------------------------------------------------------------
# Build
# ----------------------------------------------------------------------------
def main():
    out = "wingfoil-carousel.pdf"
    c = canvas.Canvas(out, pagesize=(W, H))
    for fn in [slide_01, slide_02, slide_03, slide_04, slide_05,
               slide_06, slide_07, slide_08, slide_09, slide_10, slide_11]:
        fn(c)
        c.showPage()
    c.save()
    print(f"wrote {out}")


if __name__ == "__main__":
    main()

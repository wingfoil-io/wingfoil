#!/usr/bin/env python3
"""Strategy loop: the execution layer in one graph, wired from Python.

The Rust `execution_strategy_loop` example, port for port. A toy quoter rests
one lot a tick outside the touch on one future. Its desireds go into the OMS;
what the OMS sends crosses a millisecond of wire through the one feedback cut,
past a five-lot ceiling, into the test venue, whose reports come back up the
same wire. Fills fold into a book reconciled against the venue's holdings, and
a kill switch latches on a mismatch or on the day's loss. Its halt is the
OMS's cancel-all and the quoter's reason to stop, until an operator clears it.

Prices and quantities are `decimal.Decimal`; times are nanoseconds. The run is
historical, so the output below is exact and the same on every run.

    cd crates/wingfoil-python && maturin develop
    python examples/strategy_loop.py
"""

from decimal import Decimal as D

import wingfoil as wf

MS = 1_000_000
TICK = D("0.25")
OPENING_EQUITY = D("100000")
DAILY_LOSS = 0.02


def at(graph, rows):
    """A source that emits `rows[ms]` at each listed millisecond."""
    return graph.counter(MS).with_time().filter_map(
        lambda tv: rows.get(tv[0] // MS) if tv[0] % MS == 0 else None
    )


def touch(bid, ask):
    return [{"instrument": "ES", "bid": D(bid), "ask": D(ask)}]


g = wf.Graph()

# The market at the venue trades down through the bid at 100ms and recovers.
touches = at(
    g,
    {
        0: touch("5000.00", "5000.25"),
        100: touch("4999.50", "4999.75"),
        420: touch("5000.00", "5000.25"),
        460: touch("5000.00", "5000.25"),
    },
)
# At 300ms the venue says it holds two lots where the fold has one.
holdings = at(g, {300: {"whole": True, "held": {"ES": D(2)}, "as_of": 300 * MS}})
# The operator clears the switch at 450ms.
clear = at(g, {450: True})
sweep = g.counter(50 * MS)
seen = touches.delay(MS)

# The one cut: what the OMS sends at one instant lands a wire later.
landed, cut = g.feedback()
session = wf.execution_sim_venue(landed.delay(MS), touches, cap=D(5))
reports = session["reports"].delay(MS)
fills = reports.filter_map(lambda rs: [r for r in rs if r["type"] == "fill"] or None)
marks = seen.map(lambda ts: {t["instrument"]: (t["bid"] + t["ask"]) / 2 for t in ts})
mismatches, desk = wf.execution_reconcile(
    fills,
    holdings,
    sweep,
    D(50),  # the E-mini S&P: $50 a point
    100 * MS,
    settlements=session["settlements"].delay(MS),
    marks=marks,
)


def assess(switch, now, desk):
    """The caps over one desk reading. An equity nobody could state breaches."""
    breached = set()
    pnls = [p["pnl"] for p in desk["positions"]]
    equity = None if None in pnls else OPENING_EQUITY + sum((p["net"] for p in pnls), D(0))
    loss = switch.daily_loss(now, equity)
    if loss is None or not loss[0] <= DAILY_LOSS:
        breached.add("daily_loss")
    if desk["mismatches"]:
        breached.add("mismatch")
    return breached


assessed = wf.execution_kill_switch(desk, assess, ["daily_loss", "mismatch"], clear=clear)
stance = assessed.map(lambda a: a["stance"])
cancel_all = stance.distinct().filter_map(lambda s: True if s == "halted" else None)


def quote(now, t, stance):
    """One lot a tick outside the touch, or nothing unless the switch is open."""
    if stance == "open" and t["bid"] is not None and t["ask"] is not None:
        return {
            "instrument": "ES",
            "as_of": now,
            "bids": [(t["bid"] - TICK, 1)],
            "asks": [(t["ask"] + TICK, 1)],
        }
    return {"instrument": "ES", "as_of": now}


desired = seen.with_time().join_passive(stance, lambda tv, st: [quote(tv[0], t, st) for t in tv[1]])
requests, _pacing = wf.execution_oms(
    desired,
    reports,
    sweep,
    rate=(50, 50),
    cancel_all_rate=(1, 1),
    max_desired_age_nanos=60_000 * MS,
    cancel_all=cancel_all,
)
requests = requests.feedback(cut)


def describe(r):
    if r["type"] == "place":
        return f"place  {r['id']} {r['side']} {r['qty']} @ {r['price']}"
    if r["type"] == "amend":
        return f"amend  {r['order']} -> {r['qty']} @ {r['price']}"
    if r["type"] == "cancel":
        return f"cancel {r['order']}"
    return "cancel all"


def heard(r):
    if r["type"] == "fill":
        return f"fill      {r['order']} {r['qty']} @ {r['price']} ({r['liquidity']})"
    return f"{r['type']:<9} {r['order']}"


def each(stream, line):
    stream.with_time().for_each(lambda tv: [print(f"t={tv[0] // MS:>3}ms {line(v)}") for v in tv[1]])


each(requests, lambda r: f"sent   {describe(r)}")
each(reports, lambda r: f"heard  {heard(r)}")
each(mismatches, lambda m: f"desk   mismatch: ours {m['ours']}, venue {m['venue']} → re-based")
assessed.map(lambda a: (a["stance"], tuple(a["standing"]), tuple(a["latched"]))).distinct().with_time().for_each(
    lambda tv: print(
        f"t={tv[0] // MS:>3}ms switch {tv[1][0]}: standing {list(tv[1][1])}, latched {list(tv[1][2])}"
    )
)

g.run(duration_nanos=500 * MS)

(position,) = desk.value()["positions"]
print(
    f"end    net {position['net']} entry {position['entry']} "
    f"mark {position['mark']} pnl {position['pnl']['net']}"
)

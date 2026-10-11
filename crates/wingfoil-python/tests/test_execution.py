"""Tests for the execution layer's Python bindings.

All of these run **by default**. The execution layer does no I/O: the venue
is the in-process test venue, and every run is historical from time zero, so
each assertion pins exact values and engine times. There is no marked tier
and no integration workflow, by design.

`test_strategy_loop_matches_the_rust_example` is the anchor. It wires the
`execution_strategy_loop` Rust example's graph from Python and asserts the
same requests, reports, switch transitions and final PnL the Rust example
prints.
"""

from decimal import Decimal as D

import pytest

import wingfoil as wf

MS = 1_000_000


def at(graph, rows):
    """A source emitting `rows[ms]` at each listed millisecond."""
    return graph.counter(MS).with_time().filter_map(
        lambda tv: rows.get(tv[0] // MS) if tv[0] % MS == 0 else None
    )


def recorder(stream):
    """Collect `(ms, value)` for every tick, as the run goes."""
    seen = []
    stream.with_time().for_each(lambda tv: seen.append((tv[0] // MS, tv[1])))
    return seen


def touch(bid, ask, instrument="ES"):
    return {"instrument": instrument, "bid": D(bid), "ask": D(ask)}


def place(order_id, side, qty, price, kind="limit", tif="gtc"):
    return {
        "type": "place",
        "id": order_id,
        "instrument": "ES",
        "side": side,
        "qty": qty,
        "price": D(price),
        "kind": kind,
        "tif": tif,
        "created": 0,
    }


def fill(order_id, side, qty, price, recv_ms, exec_id=None):
    return {
        "type": "fill",
        "order": order_id,
        "exec_id": exec_id or f"x{order_id}-{recv_ms}",
        "instrument": "ES",
        "side": side,
        "qty": D(qty),
        "filled": D(qty),
        "remaining": D(0),
        "price": D(price),
        "fee": D(0),
        "liquidity": "maker",
        "recv_time": recv_ms * MS,
    }


# ---------------------------------------------------------------------------
# The surface
# ---------------------------------------------------------------------------


def test_module_exposes_the_execution_surface():
    for name in [
        "execution_oms",
        "execution_algo",
        "execution_sim_venue",
        "execution_position",
        "execution_reconcile",
        "execution_kill_switch",
        "execution_replace_chain",
        "execution_replace_chain_sending",
        "KillSwitch",
        "FeedbackSink",
    ]:
        assert hasattr(wf, name), name


def test_feedback_delivers_one_instant_later():
    g = wf.Graph()
    looped, sink = g.feedback()
    g.counter(100).feedback(sink)
    seen = looped.with_time().accumulate()
    g.run(cycles=4)
    assert seen.value() == [(1, 1), (101, 2)]


# ---------------------------------------------------------------------------
# The whole loop
# ---------------------------------------------------------------------------


def strategy_loop():
    """The Rust `execution_strategy_loop` example, wired from Python."""
    g = wf.Graph()
    touches = at(
        g,
        {
            0: [touch("5000.00", "5000.25")],
            100: [touch("4999.50", "4999.75")],
            420: [touch("5000.00", "5000.25")],
            460: [touch("5000.00", "5000.25")],
        },
    )
    holdings = at(g, {300: {"whole": True, "held": {"ES": D(2)}, "as_of": 300 * MS}})
    clear = at(g, {450: True})
    sweep = g.counter(50 * MS)
    seen = touches.delay(MS)

    landed, cut = g.feedback()
    session = wf.execution_sim_venue(landed.delay(MS), touches, cap=D(5))
    reports = session["reports"].delay(MS)
    fills = reports.filter_map(lambda rs: [r for r in rs if r["type"] == "fill"] or None)
    marks = seen.map(lambda ts: {t["instrument"]: (t["bid"] + t["ask"]) / 2 for t in ts})
    mismatches, desk = wf.execution_reconcile(
        fills,
        holdings,
        sweep,
        D(50),
        100 * MS,
        settlements=session["settlements"].delay(MS),
        marks=marks,
    )

    def assess(switch, now, desk):
        breached = set()
        pnls = [p["pnl"] for p in desk["positions"]]
        equity = None if None in pnls else D(100000) + sum((p["net"] for p in pnls), D(0))
        loss = switch.daily_loss(now, equity)
        if loss is None or not loss[0] <= 0.02:
            breached.add("daily_loss")
        if desk["mismatches"]:
            breached.add("mismatch")
        return breached

    assessed = wf.execution_kill_switch(desk, assess, ["daily_loss", "mismatch"], clear=clear)
    stance = assessed.map(lambda a: a["stance"])
    cancel_all = stance.distinct().filter_map(lambda s: True if s == "halted" else None)

    def quote(now, t, stance):
        if stance == "open":
            return {
                "instrument": "ES",
                "as_of": now,
                "bids": [(t["bid"] - D("0.25"), 1)],
                "asks": [(t["ask"] + D("0.25"), 1)],
            }
        return {"instrument": "ES", "as_of": now}

    desired = seen.with_time().join_passive(
        stance, lambda tv, st: [quote(tv[0], t, st) for t in tv[1]]
    )
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
    return g, requests, reports, mismatches, desk, assessed


def test_strategy_loop_matches_the_rust_example():
    g, requests, reports, mismatches, desk, assessed = strategy_loop()
    sent = recorder(requests)
    heard = recorder(reports)
    found = recorder(mismatches)
    switch = recorder(
        assessed.map(lambda a: (a["stance"], tuple(a["standing"]), tuple(a["latched"]))).distinct()
    )
    g.run(duration_nanos=500 * MS)

    def brief(r):
        if r["type"] == "place":
            return ("place", r["id"], r["side"], r["qty"], r["price"])
        if r["type"] == "amend":
            return ("amend", r["order"], r["qty"], r["price"])
        return (r["type"],)

    assert [(t, brief(r)) for t, burst in sent for r in burst] == [
        (1, ("place", 1, "bid", D(1), D("4999.75"))),
        (1, ("place", 2, "ask", D(1), D("5000.5"))),
        (101, ("place", 3, "bid", D(1), D("4999.25"))),
        (101, ("amend", 2, D(1), D(5000))),
        (400, ("cancel_all",)),
        (461, ("place", 4, "bid", D(1), D("4999.75"))),
        (461, ("place", 5, "ask", D(1), D("5000.5"))),
    ]
    assert [(t, r["type"], r["order"]) for t, burst in heard for r in burst] == [
        (3, "ack", 1),
        (3, "ack", 2),
        (101, "fill", 1),
        (103, "ack", 3),
        (103, "ack", 2),
        (402, "cancelled", 3),
        (402, "cancelled", 2),
        (463, "ack", 4),
        (463, "ack", 5),
    ]
    assert found == [
        (400, [{"instrument": "ES", "ours": D(1), "venue": D(2), "since": 300 * MS}])
    ]
    assert switch == [
        (0, ("open", (), ())),
        (400, ("halted", ("mismatch",), ("mismatch",))),
        (421, ("halted", (), ("mismatch",))),
        (450, ("open", (), ())),
    ]
    (position,) = desk.value()["positions"]
    assert position["net"] == D(2)
    assert position["entry"] == D("4999.6875")
    assert position["mark"] == D("5000.125")
    assert position["pnl"]["net"] == D("43.75")
    assert desk.value()["rebased"] == 1


# ---------------------------------------------------------------------------
# The test venue and the ceiling
# ---------------------------------------------------------------------------


def venue_graph(requests_at, cap=None, **venue):
    g = wf.Graph()
    requests = at(g, requests_at)
    touches = at(g, {0: [touch("100", "101")]})
    session = wf.execution_sim_venue(requests, touches, cap=cap, **venue)
    return g, session


def test_the_sim_venue_fills_a_crossing_limit_at_the_touch():
    g, session = venue_graph({1: [place(1, "bid", 2, "101")]})
    heard = recorder(session["reports"])
    g.run(duration_nanos=5 * MS)
    ((t, (ack, trade)),) = heard
    assert t == 1
    assert ack["type"] == "ack" and ack["order"] == 1
    assert trade["type"] == "fill"
    assert (trade["qty"], trade["price"], trade["liquidity"]) == (D(2), D(101), "taker")


def test_a_venue_without_post_only_refuses_one():
    g, session = venue_graph(
        {1: [place(1, "bid", 1, "99", kind="post_only")]}, post_only=False
    )
    heard = recorder(session["reports"])
    g.run(duration_nanos=5 * MS)
    assert [r["type"] for _, burst in heard for r in burst] == ["reject"]


def test_an_order_over_the_cap_aborts_the_run():
    g, session = venue_graph({1: [place(1, "bid", 6, "99")]}, cap={"ES": 5})
    recorder(session["reports"])
    with pytest.raises(RuntimeError, match="ES"):
        g.run(duration_nanos=5 * MS)


def test_a_bad_request_aborts_the_run_naming_the_field():
    bad = place(1, "bid", 1, "99")
    bad["reduceonly"] = True
    g, session = venue_graph({1: [bad]})
    recorder(session["reports"])
    with pytest.raises(RuntimeError, match="unknown key \"reduceonly\""):
        g.run(duration_nanos=5 * MS)


def test_wiring_arguments_are_checked_at_the_call():
    g = wf.Graph()
    s = g.counter(MS)
    with pytest.raises(ValueError, match="epoch"):
        wf.execution_sim_venue(s, s, epoch=1 << 20)
    with pytest.raises(ValueError, match="cap"):
        wf.execution_sim_venue(s, s, cap="lots")
    with pytest.raises(ValueError, match="passive"):
        wf.execution_oms(s, s, s, passive="aggressive")
    with pytest.raises(ValueError, match="headroom"):
        wf.execution_oms(s, s, s, headroom=0.5)
    with pytest.raises(ValueError, match="measure"):
        wf.execution_position(s, "cubic")
    with pytest.raises(ValueError, match="named twice"):
        wf.execution_kill_switch(s, lambda *a: [], ["a", "a"])
    with pytest.raises(ValueError, match="callable"):
        wf.execution_kill_switch(s, "assess", ["a"])
    with pytest.raises(TypeError):
        wf.execution_replace_chain(s, s)  # mass_cancel is the venue's to state


# ---------------------------------------------------------------------------
# Positions
# ---------------------------------------------------------------------------


def test_the_position_fold_books_fills_and_cashflows():
    g = wf.Graph()
    fills = at(g, {1: fill(1, "bid", 2, "100", 1), 2: fill(2, "ask", 1, "110", 2)})
    cashflows = at(g, {3: {"instrument": "ES", "amount": D("-1.5"), "recv_time": 3 * MS}})
    book = wf.execution_position(fills, {"ES": 10}, cashflows=cashflows)
    g.run(duration_nanos=5 * MS)
    (es,) = book.value()
    assert es["net"] == D(1)
    assert es["realised"] == D(100)  # one lot, ten points, ten a point
    assert es["cashflows"] == D("-1.5")
    assert es["measure"] == D(10)


def test_an_unvalued_instrument_is_folded_but_not_valued():
    g = wf.Graph()
    fills = at(g, {1: fill(1, "bid", 2, "100", 1)})
    marks = at(g, {1: {"ES": 105}})
    _, desk = wf.execution_reconcile(
        fills, at(g, {}), g.counter(MS), None, 10 * MS, marks=marks
    )
    g.run(duration_nanos=3 * MS)
    (es,) = desk.value()["positions"]
    assert es["net"] == D(2)
    assert es["measure"] is None
    assert es["pnl"] is None
    assert desk.value()["unvalued"] == 1


# ---------------------------------------------------------------------------
# The kill switch
# ---------------------------------------------------------------------------


def test_a_breach_latches_and_a_clear_over_it_is_refused():
    g = wf.Graph()
    readings = at(g, {1: 10, 2: 0, 3: 10, 4: 0})
    clear = at(g, {3: True, 5: True})
    assessed = wf.execution_kill_switch(
        readings, lambda switch, now, gross: ["gross"] if gross > 5 else [], ["gross"],
        clear=clear,
    )
    seen = recorder(assessed)
    g.run(duration_nanos=6 * MS)
    by_ms = {t: a for t, a in seen}
    assert by_ms[1]["stance"] == "halted" and by_ms[1]["latched_since"] == 1 * MS
    assert by_ms[2]["stance"] == "halted" and by_ms[2]["standing"] == []
    assert by_ms[3]["cleared"] == {"still_breached": ["gross"]}
    assert by_ms[5]["cleared"] is True and by_ms[5]["stance"] == "open"


def test_an_assess_that_raises_records_every_limit_breached():
    def assess(switch, now, reading):
        raise RuntimeError("no equity")

    g = wf.Graph()
    assessed = wf.execution_kill_switch(at(g, {1: 1}), assess, ["loss", "gross"])
    g.run(duration_nanos=2 * MS)
    assert assessed.value()["latched"] == ["loss", "gross"]
    assert assessed.value()["stance"] == "halted"


def test_a_restored_switch_keeps_its_latch():
    g = wf.Graph()
    first = wf.execution_kill_switch(at(g, {1: 1}), lambda *a: ["gross"], ["gross"])
    g.run(duration_nanos=2 * MS)
    snapshot = first.value()["snapshot"]
    assert snapshot["latch"]["breaches"] == ["gross"]

    g = wf.Graph()
    restored = wf.execution_kill_switch(
        at(g, {1: 1}), lambda *a: [], ["gross"], restored=snapshot,
    )
    g.run(duration_nanos=2 * MS)
    assert restored.value()["stance"] == "halted"
    assert restored.value()["latched"] == ["gross"]


def test_daily_loss_anchors_the_day_on_its_first_equity():
    losses = []

    def assess(switch, now, equity):
        losses.append(switch.daily_loss(now, equity))
        return []

    g = wf.Graph()
    wf.execution_kill_switch(at(g, {1: D(1000), 2: D(900)}), assess, ["loss"])
    g.run(duration_nanos=3 * MS)
    assert losses == [(0.0, D(0)), (0.1, D(100))]


# ---------------------------------------------------------------------------
# Parent orders and the FIX replace chain
# ---------------------------------------------------------------------------


def test_a_sweep_parent_crosses_once_through_the_oms():
    g = wf.Graph()
    parents = at(
        g,
        {1: {"instrument": "ES", "side": "bid", "total": 3, "cap": 101, "by": 50 * MS, "style": "sweep"}},
    )
    landed, cut = g.feedback()
    clock = g.counter(MS)
    touches = at(g, {0: [touch("100", "101")]})
    session = wf.execution_sim_venue(landed, touches)
    reports = session["reports"]
    desired, progress = wf.execution_algo(parents, landed, reports, clock)
    requests, _ = wf.execution_oms(desired, reports, clock)
    requests.feedback(cut)
    done = recorder(progress)
    g.run(duration_nanos=10 * MS)
    final = done[-1][1][-1]
    assert final["status"] == "filled"
    assert final["filled"] == D(3)
    assert final["parent"]["style"] == "sweep"


def test_the_replace_chain_mints_a_clordid_per_message():
    g = wf.Graph()
    requests = at(
        g,
        {
            1: [place(1, "bid", 1, "99")],
            3: [{"type": "amend", "order": 1, "instrument": "ES", "price": 98, "qty": 1}],
        },
    )
    executions = at(
        g, {2: {"cl_ord_id": 1, "kind": "new", "venue_time": 2 * MS}}
    )
    messages, reports = wf.execution_replace_chain(requests, executions, mass_cancel=False)
    sent = recorder(messages)
    heard = recorder(reports)
    g.run(duration_nanos=5 * MS)
    (new,), (replace,) = (burst for _, burst in sent)
    assert new["type"] == "new" and replace["type"] == "replace"
    assert replace["orig"] == new["cl_ord_id"] != replace["cl_ord_id"]
    assert [r["type"] for _, burst in heard for r in burst] == ["ack"]


def test_a_refused_send_is_answered_in_the_same_instant():
    def send(message, now):
        return "rate_limited"

    g = wf.Graph()
    reports = wf.execution_replace_chain_sending(
        at(g, {1: [place(1, "bid", 1, "99")]}), at(g, {}), send, mass_cancel=True
    )
    heard = recorder(reports)
    g.run(duration_nanos=3 * MS)
    ((t, (reject,)),) = heard
    assert t == 1
    assert (reject["type"], reject["order"], reject["reason"]) == ("reject", 1, "rate_limited")

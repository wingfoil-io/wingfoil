"""Python round-trip tests for the wingfoil interop module.

These exercise the real extension module (built by maturin), proving the erased
object form is reachable and composable from Python: native values in, native
values out, Python callables wired into the graph, and run errors surfaced as
exceptions.
"""

import pytest

import wingfoil as wf


def test_constant_maps_via_python_lambda():
    g = wf.Graph()
    out = g.constant(4.0).map(lambda x: x * x)
    g.run(cycles=1)
    assert out.value() == 16.0


def test_values_source_replays_a_sequence():
    g = wf.Graph()
    out = g.values([10, 20, 30], period_nanos=100).accumulate()
    g.run(cycles=3)
    assert out.value() == [10, 20, 30]


def test_values_source_feeds_combinators():
    # Feed real data from Python, run it through a chain.
    g = wf.Graph()
    out = g.values([1, 2, 3, 4], period_nanos=100).filter_value(lambda n: n % 2 == 0).sum()
    g.run(cycles=4)
    assert out.value() == 6.0  # keep 2, 4 -> cumulative sum 6


def test_values_source_carries_strings():
    g = wf.Graph()
    out = g.values(["a", "b", "c"], period_nanos=100).map(lambda s: s.upper()).accumulate()
    g.run(cycles=3)
    assert out.value() == ["A", "B", "C"]


def test_counter_source_ticks():
    g = wf.Graph()
    doubled = g.counter(period_nanos=100).map(lambda n: n * 2)
    g.run(cycles=3)
    assert doubled.value() == 6  # 3rd tick -> 3 * 2


def test_filter_by_python_predicate():
    g = wf.Graph()
    counter = g.counter(period_nanos=100)
    keep = counter.map(lambda n: n > 2)
    filtered = counter.filter(keep)
    g.run(cycles=5)
    # last passing value is the 5th tick
    assert filtered.value() == 5


def test_distinct_suppresses_duplicates():
    g = wf.Graph()
    # 1,2,3,4 -> n//2 -> 0,1,1,2 -> distinct passes changes only; final value 2
    stepped = g.counter(period_nanos=100).map(lambda n: n // 2).distinct()
    g.run(cycles=4)
    assert stepped.value() == 2


def test_drop_small_change_compares_against_last_emitted():
    g = wf.Graph()
    # 1..6 -> n * 3 -> 3,6,9,12,15,18; a step under 8 is "small". The
    # comparison is against the last *emitted* value, so 9 (9-3=6) is still
    # dropped and the drift only ticks again at 12.
    out = (
        g.counter(period_nanos=100)
        .map(lambda n: n * 3)
        .drop_small_change(lambda cur, prev: abs(cur - prev) < 8)
        .collect()
    )
    g.run(cycles=6)
    assert out.value() == [(0, 3), (300, 12)]


def test_drop_small_change_matches_distinct_with_equality():
    # An equality predicate degenerates to `distinct` — the same round-trip
    # oracle the Rust catalog test uses.
    g = wf.Graph()
    stepped = g.counter(period_nanos=100).map(lambda n: n // 2)
    dropped = stepped.drop_small_change(lambda cur, prev: cur == prev).accumulate()
    distinct = stepped.distinct().accumulate()
    g.run(cycles=6)
    assert dropped.value() == [0, 1, 2, 3]
    assert dropped.value() == distinct.value()


def test_drop_small_change_predicate_exception_aborts_run():
    g = wf.Graph()
    g.counter(period_nanos=100).drop_small_change(lambda cur, prev: cur.no_such_attr)
    with pytest.raises(RuntimeError, match="Python drop_small_change predicate raised"):
        g.run(cycles=3)


def test_drop_small_change_non_bool_return_aborts_run():
    g = wf.Graph()
    g.counter(period_nanos=100).drop_small_change(lambda cur, prev: "not a bool")
    with pytest.raises(RuntimeError, match="must return a bool"):
        g.run(cycles=3)


def test_map_callable_exception_aborts_run():
    g = wf.Graph()
    g.constant("not a number").map(lambda x: x + 1)
    with pytest.raises(RuntimeError, match="Python map callable raised"):
        g.run(cycles=1)


def test_run_can_be_bounded_by_a_duration():
    """`cycles` is not the only bound — `duration_nanos` stops on graph time.

    The bound is legacy's: a run ends on the cycle whose elapsed time *exceeds*
    it, and the check happens between cycles — so a counter gets one tick past
    `duration_nanos` before the loop notices.
    """

    def tick_times(duration_nanos):
        g = wf.Graph()
        out = g.counter(period_nanos=100).collect()
        g.run(duration_nanos=duration_nanos)
        return [time for time, _ in out.value()]

    assert [0] == tick_times(0)
    assert [0, 100, 200] == tick_times(100)
    assert [0, 100, 200, 300, 400] == tick_times(300)


def test_cycles_wins_when_both_bounds_are_given():
    """Legacy raised on the conflicting pair; next resolves it, cycles first."""
    g = wf.Graph()
    out = g.counter(period_nanos=100).accumulate()
    g.run(cycles=2, duration_nanos=10_000)
    assert out.value() == [1, 2]


def test_start_nanos_offsets_the_historical_clock():
    g = wf.Graph()
    out = g.counter(period_nanos=100).collect()
    g.run(cycles=3, start_nanos=1_000)
    assert [(1000, 1), (1100, 2), (1200, 3)] == out.value()


def test_value_is_none_before_a_stream_has_ticked():
    """The empty element reads back as Python `None`, never a panic — before
    the graph has run at all (legacy `peek_value`'s answer), and after a run in
    which the stream never ticked."""
    g = wf.Graph()
    quiet = g.counter(period_nanos=100).filter_value(lambda n: False)
    assert quiet.value() is None
    g.run(cycles=3)
    assert quiet.value() is None


def test_graph_reruns_and_resets():
    # A re-runnable graph can be run repeatedly; the engine resets node state
    # between runs, so it reproduces the same values.
    g = wf.Graph()
    out = g.constant(2.0).map(lambda x: x + 1)
    g.run(cycles=1)
    assert out.value() == 3.0
    g.run(cycles=1)
    assert out.value() == 3.0


def test_native_types_round_trip():
    g = wf.Graph()
    out = g.constant("hello").map(lambda s: s.upper())
    g.run(cycles=1)
    assert out.value() == "HELLO"


def test_user_op_scale():
    # `scale` is a Rust-authored op (via pyop!), called as a free function.
    g = wf.Graph()
    out = wf.scale(g.constant(3.0), 4.0)
    g.run(cycles=1)
    assert out.value() == 12.0


def test_user_op_composes_with_builtins():
    # The "extend in Python" thesis: a Rust user op wired between built-in
    # combinators. counter 1..5 -> scale x10 -> 10,20,30,40,50 -> keep > 25.
    g = wf.Graph()
    scaled = wf.scale(g.counter(period_nanos=100), 10.0)
    keep = scaled.map(lambda x: x > 25)
    out = scaled.filter(keep)
    g.run(cycles=5)
    assert out.value() == 50.0


def test_pyop_macro_op_square():
    # `square` is generated by the #[pyop] proc macro from an Op impl.
    g = wf.Graph()
    out = wf.square(g.constant(5.0))
    g.run(cycles=1)
    assert out.value() == 25.0


def test_pyop_stateful_running_total():
    # running_total is a stateful #[pyop] (accumulator in the op's State).
    g = wf.Graph()
    out = wf.running_total(g.counter(period_nanos=100))  # 1,2,3 -> 1,3,6
    g.run(cycles=3)
    assert out.value() == 6.0


def test_pyop_stateful_state_reseeds_on_rerun():
    g = wf.Graph()
    out = wf.running_total(g.counter(period_nanos=100))
    g.run(cycles=3)
    assert out.value() == 6.0
    g.run(cycles=3)  # engine re-seeds State from Default (0.0); restart, not continue
    assert out.value() == 6.0


def test_pyadapter_source():
    # ramp_source is a Rust source adapter (#[pyadapter]) exposed as a callable.
    g = wf.Graph()
    out = wf.ramp_source(g, 10.0, 2.0).accumulate()
    g.run(cycles=3)
    # start=10, step=2 -> 10, 12, 14
    assert out.value() == [10.0, 12.0, 14.0]


def test_pyadapter_sink():
    # list_sink is a Rust sink adapter (#[pyadapter], no source marker): it
    # consumes the stream into a Python list.
    out = []
    g = wf.Graph()
    wf.list_sink(g.counter(period_nanos=100), out)  # counter 1,2,3 -> f64
    g.run(cycles=3)
    assert out == [1.0, 2.0, 3.0]


def test_pyadapter_source_and_sink_together():
    out = []
    g = wf.Graph()
    wf.list_sink(wf.ramp_source(g, 10.0, 5.0), out)  # 10, 15, 20
    g.run(cycles=3)
    assert out == [10.0, 15.0, 20.0]


def test_pyadapter_burst_source():
    # pair_source is a burst-shaped source adapter: each tick is a Burst<f64> of
    # two same-instant values, which erases to a Python list.
    g = wf.Graph()
    out = wf.pair_source(g).accumulate()
    g.run(cycles=3)
    assert out.value() == [[1.0, 10.0], [2.0, 20.0], [3.0, 30.0]]


def test_pyadapter_burst_round_trip():
    # A burst source's per-tick list feeds a burst sink, which rebuilds the
    # multi-value burst: Burst -> Python list -> Burst.
    out = []
    g = wf.Graph()
    wf.burst_list_sink(wf.pair_source(g), out)  # [1,10],[2,20],[3,30]
    g.run(cycles=3)
    assert out == [[1.0, 10.0], [2.0, 20.0], [3.0, 30.0]]


def test_pyadapter_burst_sink_scalar_input():
    # A plain scalar stream arrives at a burst sink as single-element bursts.
    out = []
    g = wf.Graph()
    wf.burst_list_sink(g.counter(period_nanos=100), out)  # 1,2,3 -> [1],[2],[3]
    g.run(cycles=3)
    assert out == [[1.0], [2.0], [3.0]]


def test_pyadapter_source_feeds_combinators():
    g = wf.Graph()
    out = wf.ramp_source(g, 1.0, 1.0).sum()  # 1,2,3 -> cumulative 1,3,6
    g.run(cycles=3)
    assert out.value() == 6.0


def test_pygraph_reuses_a_rust_subgraph():
    # doubled_running_total is a Rust-authored sub-graph (#[pygraph]) spliced in.
    g = wf.Graph()
    out = wf.doubled_running_total(g.counter(period_nanos=100))
    g.run(cycles=3)
    # 1,2,3 -> double -> 2,4,6 -> cumulative sum -> 2,6,12
    assert out.value() == 12.0


def test_pyop_two_input_op():
    # weighted_add is a two-input #[pyop]: module.weighted_add(stream, other).
    g = wf.Graph()
    a = g.counter(period_nanos=100)  # 1,2,3
    b = g.counter(period_nanos=100).map(lambda n: n * 10)  # 10,20,30
    out = wf.weighted_add(a, b)
    g.run(cycles=3)
    assert out.value() == 33.0  # 3 + 30


def test_pyop_three_input_op():
    # blend3 is a three-input #[pyop]: module.blend3(stream, second, third),
    # the join3 shape over the wire_op3 seam.
    g = wf.Graph()
    a = g.counter(period_nanos=100)  # 1,2,3
    b = a.map(lambda n: n * 2)  # 2,4,6
    c = a.map(lambda n: n * 3)  # 3,6,9
    out = wf.blend3(a, b, c).collect()
    g.run(cycles=3)
    # a + 10b + 100c, with tick times
    assert [(0, 321.0), (100, 642.0), (200, 963.0)] == out.value()


def test_pyop_three_inputs_are_all_active():
    """Any of the three ticking activates the op — none is a passive read."""
    g = wf.Graph()
    fast = g.counter(period_nanos=100)
    slow = g.counter(period_nanos=300)
    out = wf.blend3(fast, slow, slow).collect()
    g.run(cycles=4)
    # `fast` alone ticks at t=100 and t=200, so the op runs on every cycle.
    assert 4 == len(out.value())


def test_pyop_four_input_op():
    # blend4 is a four-input #[pyop] — the widest arity the macro emits,
    # over module.blend4(stream, second, third, fourth).
    g = wf.Graph()
    a = g.counter(period_nanos=100)  # 1,2,3
    b = a.map(lambda n: n * 2)
    c = a.map(lambda n: n * 3)
    d = a.map(lambda n: n * 4)
    out = wf.blend4(a, b, c, d).collect()
    g.run(cycles=3)
    # a + 10b + 100c + 1000d
    assert [(0, 4321.0), (100, 8642.0), (200, 12963.0)] == out.value()


def test_pyop_four_input_names_its_streams():
    """The generated stream parameters are named, so they can be passed by
    keyword — `other` at arity two, `second`/`third`/`fourth` beyond it."""
    g = wf.Graph()
    a = g.counter(period_nanos=100)
    b = a.map(lambda n: n * 2)
    c = a.map(lambda n: n * 3)
    d = a.map(lambda n: n * 4)
    out = wf.blend4(a, second=b, third=c, fourth=d).collect()
    g.run(cycles=1)
    assert [(0, 4321.0)] == out.value()


def test_pyop_two_input_keeps_its_original_parameter_name():
    g = wf.Graph()
    a = g.counter(period_nanos=100)
    b = a.map(lambda n: n * 10)
    out = wf.weighted_add(a, other=b)
    g.run(cycles=3)
    assert out.value() == 33.0


def test_pyop_tuple_cfg_names_each_element():
    # clamped_scale declares `arg = (factor, ceiling)` over `Cfg = (f64, f64)`,
    # so each knob is its own Python parameter rather than one tuple argument.
    g = wf.Graph()
    out = wf.clamped_scale(g.counter(period_nanos=100), 10.0, 25.0).collect()
    g.run(cycles=3)
    # 1,2,3 -> x10 -> 10,20,30 -> clamped at 25
    assert [(0, 10.0), (100, 20.0), (200, 25.0)] == out.value()


def test_pyop_tuple_cfg_accepts_keywords():
    g = wf.Graph()
    out = wf.clamped_scale(
        g.counter(period_nanos=100), factor=10.0, ceiling=25.0
    ).collect()
    g.run(cycles=3)
    assert [(0, 10.0), (100, 20.0), (200, 25.0)] == out.value()


def test_pyop_and_pyop_fn_compose():
    # Both macro forms, chained: counter -> square -> scale x2.
    g = wf.Graph()
    out = wf.scale(wf.square(g.counter(period_nanos=100)), 2.0)
    g.run(cycles=3)  # 3rd tick: 3^2 * 2
    assert out.value() == 18.0


def test_custom_node_python_object_as_graph_node():
    # A Python object acting as a graph node (the legacy CustomStream shape):
    # sum two upstream counters each cycle via the cycle(values)/peek protocol.
    class Adder:
        def __init__(self):
            self.total = 0

        def cycle(self, values):
            self.total = sum(values)
            return True

        def peek(self):
            return self.total

    g = wf.Graph()
    a = g.counter(period_nanos=100)
    b = g.counter(period_nanos=100)
    summed = g.custom_node([a, b], Adder())
    g.run(cycles=3)
    assert summed.value() == 6  # 3rd tick: 3 + 3


def test_custom_node_can_stay_quiet():
    # Returning False from cycle suppresses the tick (legacy "did I tick?").
    class Evens:
        def __init__(self):
            self.v = 0

        def cycle(self, values):
            self.v = values[0]
            return self.v % 2 == 0

        def peek(self):
            return self.v

    g = wf.Graph()
    evens = g.custom_node([g.counter(period_nanos=100)], Evens())
    g.run(cycles=6)
    assert evens.value() == 6  # last even value passed through


def test_custom_node_exception_aborts_run():
    class Boom:
        def cycle(self, values):
            raise ValueError("boom")

        def peek(self):
            return 0

    g = wf.Graph()
    g.custom_node([g.counter(period_nanos=100)], Boom())
    with pytest.raises(RuntimeError, match="Python custom node cycle raised"):
        g.run(cycles=1)


def test_count_ignores_values():
    g = wf.Graph()
    out = g.counter(period_nanos=100).map(lambda n: "x").count()
    g.run(cycles=3)
    assert out.value() == 3


def test_limit_caps_ticks():
    g = wf.Graph()
    out = g.counter(period_nanos=100).limit(2)
    g.run(cycles=5)
    assert out.value() == 2  # last value passed before the cap


def test_difference_of_counter_is_one():
    g = wf.Graph()
    out = g.counter(period_nanos=100).difference()
    g.run(cycles=4)
    assert out.value() == 1  # 1,2,3,4 -> deltas 1,1,1


def test_delay_re_emits_each_value_later():
    g = wf.Graph()
    out = g.counter(period_nanos=100).delay(200).collect()
    g.run(cycles=5)
    # Ticks at 0,100,200,300,400 carrying 1..5; each re-emitted 200ns later, so
    # only the first three land inside the run.
    assert [(200, 1), (300, 2), (400, 3)] == out.value()


def test_merge_lets_the_earliest_supplied_input_win():
    g = wf.Graph()
    fast = g.counter(period_nanos=100)  # 1,2,3 at t=0,100,200
    slow = g.counter(period_nanos=200).map(lambda n: n * 10)  # 10,20 at t=0,200
    out = fast.merge(slow).collect()
    g.run(cycles=3)
    # Both tick at 0 and 200; `fast` is the receiver, so it wins the tie.
    assert [(0, 1), (100, 2), (200, 3)] == out.value()


def test_not_negates_value():
    # `not` is arithmetic negation (__neg__) and is a Python keyword.
    g = wf.Graph()
    out = getattr(g.constant(5), "not")()
    g.run(cycles=1)
    assert out.value() == -5


def test_sample_emits_held_value_on_trigger():
    # A constant (ticks once) sampled on every counter tick re-emits its value.
    g = wf.Graph()
    held = g.constant(42)
    out = held.sample(g.counter(period_nanos=100))
    g.run(cycles=3)
    assert out.value() == 42


def test_throttle_rate_limits():
    # counter ticks 1..9 at 100..900; throttle(250) emits at 100, 400, 700.
    g = wf.Graph()
    out = g.counter(period_nanos=100).throttle(250)
    g.run(cycles=9)
    assert out.value() == 7  # last emission at t=700


def test_inspect_taps_and_passes_through():
    seen = []
    g = wf.Graph()
    out = g.counter(period_nanos=100).inspect(seen.append)
    g.run(cycles=3)
    assert seen == [1, 2, 3]
    assert out.value() == 3


def test_inspect_exception_aborts_run():
    def boom(v):
        raise ValueError("boom")

    g = wf.Graph()
    g.counter(period_nanos=100).inspect(boom)
    with pytest.raises(RuntimeError, match="Python inspect callable raised"):
        g.run(cycles=1)


def test_accumulate_grows_a_list():
    g = wf.Graph()
    out = g.counter(period_nanos=100).accumulate()
    g.run(cycles=3)
    assert out.value() == [1, 2, 3]


def test_print_passes_through_values():
    # `print` is a stdout debug tap; the value stream is unchanged.
    g = wf.Graph()
    out = g.counter(period_nanos=100).print().accumulate()
    g.run(cycles=3)
    assert out.value() == [1, 2, 3]


def test_logged_passes_through_values():
    # `logged` is a debug tap through the `log` crate; pass-through by design.
    g = wf.Graph()
    out = g.counter(period_nanos=100).logged("count").accumulate()
    g.run(cycles=3)
    assert out.value() == [1, 2, 3]


def test_logged_accepts_explicit_level():
    g = wf.Graph()
    out = g.counter(period_nanos=100).logged("count", "warn").accumulate()
    g.run(cycles=2)
    assert out.value() == [1, 2]


def test_logged_rejects_unknown_level():
    g = wf.Graph()
    with pytest.raises(ValueError):
        g.counter(period_nanos=100).logged("count", "verbose")


def test_buffer_flushes_at_capacity():
    g = wf.Graph()
    out = g.counter(period_nanos=100).buffer(2)
    g.run(cycles=4)
    assert out.value() == [3, 4]  # last full flush


def test_window_flushes_on_interval():
    g = wf.Graph()
    out = g.counter(period_nanos=100).window(interval_nanos=200)
    g.run(cycles=6)
    assert out.value() == [5, 6]  # last window boundary flush


def test_with_time_pairs_nanos_and_value():
    g = wf.Graph()
    out = g.counter(period_nanos=100).with_time()
    g.run(cycles=3)
    assert out.value() == (200, 3)  # ticks at t=0,100,200


def test_collect_gathers_time_value_tuples():
    g = wf.Graph()
    out = g.counter(period_nanos=100).collect()
    g.run(cycles=2)
    assert out.value() == [(0, 1), (100, 2)]


def test_fold_accumulates():
    g = wf.Graph()
    out = g.counter(period_nanos=100).fold(0, lambda acc, v: acc + v)
    g.run(cycles=3)
    assert out.value() == 6  # 1+2+3


def test_fold_restarts_on_rerun():
    g = wf.Graph()
    out = g.counter(period_nanos=100).fold(0, lambda acc, v: acc + v)
    g.run(cycles=3)
    assert out.value() == 6
    g.run(cycles=3)  # engine re-seeds the accumulator; restart, not continue
    assert out.value() == 6


def test_fold_exception_aborts_run():
    def boom(acc, v):
        raise ValueError("boom")

    g = wf.Graph()
    g.counter(period_nanos=100).fold(0, boom)
    with pytest.raises(RuntimeError, match="Python fold callable raised"):
        g.run(cycles=1)


def test_filter_map_keeps_non_none():
    g = wf.Graph()
    out = g.counter(period_nanos=100).filter_map(
        lambda n: n * 10 if n % 2 == 0 else None
    )
    g.run(cycles=4)
    assert out.value() == 40  # even counts scaled: 20, 40


def test_filter_value_keeps_on_predicate():
    g = wf.Graph()
    out = g.counter(period_nanos=100).filter_value(lambda n: n > 2)
    g.run(cycles=5)
    assert out.value() == 5  # 3, 4, 5 pass


def test_filter_value_predicate_exception_aborts_run():
    g = wf.Graph()
    g.counter(period_nanos=100).filter_value(lambda n: n.no_such_attr)
    with pytest.raises(RuntimeError, match="Python filter_value predicate raised"):
        g.run(cycles=1)


def test_filter_value_uses_python_truthiness():
    """A non-bool return is *truthiness*-tested, not rejected — the deviation
    from legacy, whose `filter` raised on anything that was not a bool. The
    strict edge is `filter`, which reads a condition *stream* as bool."""
    g = wf.Graph()
    out = g.counter(period_nanos=100).filter_value(lambda n: "" if n % 2 else "keep")
    g.run(cycles=4)
    assert out.value() == 4  # the even ticks return a non-empty (truthy) str


def test_filter_rejects_a_non_bool_condition():
    g = wf.Graph()
    counter = g.counter(period_nanos=100)
    counter.filter(counter.map(lambda n: "not a bool"))
    with pytest.raises(RuntimeError, match="not a bool"):
        g.run(cycles=1)


def test_filter_none_drops_python_none():
    g = wf.Graph()
    out = g.counter(period_nanos=100).map(
        lambda n: n if n % 2 == 0 else None
    ).filter_none()
    g.run(cycles=6)
    assert out.value() == 6  # 2, 4, 6 pass


def test_sum_is_cumulative():
    g = wf.Graph()
    out = g.counter(period_nanos=100).sum()
    g.run(cycles=4)
    assert out.value() == 10.0  # 1+2+3+4


def test_mean_and_average_are_cumulative():
    g = wf.Graph()
    out = g.counter(period_nanos=100).mean()
    avg = g.counter(period_nanos=100).average()
    g.run(cycles=4)
    assert out.value() == 2.5  # (1+2+3+4)/4
    assert avg.value() == 2.5  # average is an alias for mean


def test_sum_of_non_numeric_aborts_run():
    g = wf.Graph()
    g.constant("x").sum()
    with pytest.raises(RuntimeError, match="not a f64"):
        g.run(cycles=1)


def test_merge_all_combines_streams():
    g = wf.Graph()
    a = g.counter(period_nanos=300)  # ticks at 0,300,600
    b = g.counter(period_nanos=300).map(lambda n: n + 100)
    c = g.counter(period_nanos=300).map(lambda n: n + 200)
    out = a.merge_all([b, c])
    g.run(cycles=3)  # all tick together; earliest-supplied (a) wins the tie
    assert out.value() == 3  # a's running count


def test_dataframe_from_stream():
    pd = pytest.importorskip("pandas")
    g = wf.Graph()
    df = g.counter(period_nanos=100).dataframe()
    g.run(cycles=3)
    frame = df.value()
    assert isinstance(frame, pd.DataFrame)
    assert list(frame["time"]) == [0, 100, 200]
    assert list(frame["value"]) == [1, 2, 3]


def test_reduce_runs_from_first_value():
    g = wf.Graph()
    out = g.counter(period_nanos=100).reduce(lambda acc, v: acc + v)
    g.run(cycles=3)
    assert out.value() == 6  # 1, then 1+2=3, then 3+3=6


def test_split_decomposes_tuples():
    g = wf.Graph()
    pairs = g.counter(period_nanos=100).map(lambda n: (n, n * 10))
    left, right = pairs.split()
    g.run(cycles=3)
    assert left.value() == 3
    assert right.value() == 30


def test_bimap_combines_two_inputs():
    g = wf.Graph()
    a = g.counter(period_nanos=100)  # 1,2,3
    b = g.counter(period_nanos=100).map(lambda n: n * 10)  # 10,20,30
    out = a.bimap(b, lambda x, y: x + y)
    g.run(cycles=3)
    assert out.value() == 33  # 3 + 30


def test_bimap_exception_aborts_run():
    def boom(x, y):
        raise ValueError("boom")

    g = wf.Graph()
    a = g.counter(period_nanos=100)
    b = g.counter(period_nanos=100)
    a.bimap(b, boom)
    with pytest.raises(RuntimeError, match="Python bimap callable raised"):
        g.run(cycles=1)


def test_pygraph_multi_input_multi_output():
    """Two streams in, a tuple of two streams out — each wired onward."""
    g = wf.Graph()
    bid = g.counter(period_nanos=100).map(lambda n: float(n))
    ask = g.counter(period_nanos=100).map(lambda n: float(n) + 4.0)
    spread, mid = wf.spread_and_mid(bid, ask)
    spreads = spread.collect()
    mids = mid.collect()
    g.run(cycles=3)
    assert [(0, 4.0), (100, 4.0), (200, 4.0)] == spreads.value()
    assert [(0, 3.0), (100, 4.0), (200, 5.0)] == mids.value()


def test_pygraph_outputs_chain_like_any_stream():
    g = wf.Graph()
    bid = g.counter(period_nanos=100).map(lambda n: float(n))
    ask = g.counter(period_nanos=100).map(lambda n: float(n) + 4.0)
    _, mid = wf.spread_and_mid(bid, ask)
    out = mid.map(lambda v: v * 2).collect()
    g.run(cycles=2)
    assert [(0, 6.0), (100, 8.0)] == out.value()


def test_compiled_island_matches_its_interpreted_twin():
    """The island's interior is monomorphized straight-line code; Python wires
    around it. It must produce identical values *and* tick times."""
    g = wf.Graph()
    source = g.counter(period_nanos=100).map(lambda n: float(n))
    island = wf.compiled_island(g, source).collect()
    twin = wf.interpreted_twin(source).collect()
    g.run(cycles=4)
    # (2n + 1)^2 -> 9, 25, 49, 81
    assert [(0, 9.0), (100, 25.0), (200, 49.0), (300, 81.0)] == island.value()
    assert twin.value() == island.value()


def test_compiled_island_composes_with_dynamic_python_wiring():
    """The point of the island: a compiled interior with Python on both sides."""
    g = wf.Graph()
    source = g.counter(period_nanos=100).map(lambda n: float(n))
    out = (
        wf.compiled_island(g, source.map(lambda v: v + 1.0))
        .map(lambda v: v / 2.0)
        .collect()
    )
    g.run(cycles=2)
    # n+1 -> (2(n+1)+1)^2 -> /2
    assert [(0, 12.5), (100, 24.5)] == out.value()


def test_a_tuple_returning_source_adapter_gives_a_tuple_of_streams():
    """`#[pyadapter]` accepts a tuple return — the `(data, status)` shape.

    `split_source` wires once and hands back two streams; both are ordinary
    `Stream`s on the same graph, so Python composes onward from either.
    """
    g = wf.Graph()
    result = wf.split_source(g)
    assert isinstance(result, tuple)
    assert 2 == len(result)
    values, even = result
    assert isinstance(values, wf.Stream)
    assert isinstance(even, wf.Stream)

    seen_values, seen_even = [], []
    values.inspect(seen_values.append)
    even.inspect(seen_even.append)
    g.run(cycles=3)

    assert [1.0, 2.0, 3.0] == seen_values
    assert [False, True, False] == seen_even

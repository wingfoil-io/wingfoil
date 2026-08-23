"""Tests for the wingfoil latency Python bindings.

All of these run **by default** — there is no marked tier and no external
service. Latency stamping is pure in-process work: a graph stamps the wall
clock into a record as values flow through it, and a report sink aggregates
the per-hop deltas. The clock is the only "outside" involved, and every
assertion here is about ordering or counts rather than absolute durations, so
the suite is deterministic under a historical run.

The parity oracle is legacy `wingfoil-python`'s `tests/test_latency.py`; the
deviations from it are numbered in the module docs of
`src/latency.rs`. The two visible here: these are free functions
(`wf.stamp(stream, ...)`, not `stream.stamp(...)`), and `latency_report`
returns `(sink, stats)` rather than only the sink.
"""

import pytest

import wingfoil as wf

TICK_NANOS = 1_000_000


def traced_graph(stages, count=3):
    """A graph ticking `count` `TracedBytes`, each with a fresh record."""
    graph = wf.Graph()
    values = [wf.TracedBytes(b"payload", wf.Latency(stages)) for _ in range(count)]
    return graph, graph.values(values, TICK_NANOS)


def stamps_of(stream):
    return stream.value().latency.stamps


# ---------------------------------------------------------------------------
# Latency — the record
# ---------------------------------------------------------------------------


def test_module_exposes_the_latency_surface():
    for name in (
        "Latency",
        "TracedBytes",
        "LatencyStats",
        "stamp",
        "stamp_if",
        "stamp_precise",
        "stamp_precise_if",
        "stamp_as",
        "stamp_all",
        "latency_report",
        "latency_report_if",
    ):
        assert hasattr(wf, name), f"missing {name}"


def test_a_new_record_is_unstamped():
    record = wf.Latency(["a", "b", "c"])
    assert record.stages == ["a", "b", "c"]
    assert record.stamps == [0, 0, 0]
    assert len(record) == 3


def test_stamps_are_addressed_by_stage_name():
    record = wf.Latency(["a", "b", "c"])
    record["a"] = 100
    record["b"] = 200
    assert record["a"] == 100
    assert record["b"] == 200
    assert record["c"] == 0
    assert record.stamps == [100, 200, 0]


def test_repr_names_every_stage():
    record = wf.Latency(["a", "b"])
    record["a"] = 7
    record["b"] = 9
    assert repr(record) == "Latency(a=7, b=9)"


def test_an_empty_stage_list_raises():
    with pytest.raises(ValueError):
        wf.Latency([])


def test_a_duplicate_stage_name_raises():
    with pytest.raises(ValueError):
        wf.Latency(["a", "a"])


def test_an_unknown_stage_raises_key_error():
    record = wf.Latency(["a"])
    with pytest.raises(KeyError):
        _ = record["missing"]
    with pytest.raises(KeyError):
        record["missing"] = 5


def test_the_wire_header_round_trips():
    record = wf.Latency(["a", "b"])
    record["a"] = 12345
    record["b"] = 67890
    data = record.to_bytes()
    assert isinstance(data, bytes)
    assert len(data) == 16  # 2 stages * 8 bytes

    restored = wf.Latency.from_bytes(data, ["a", "b"])
    assert restored.stamps == [12345, 67890]
    assert restored["a"] == 12345


def test_a_trailing_payload_does_not_disturb_the_header():
    """The shape an adapter uses: an 8-byte-per-stage header, then the body."""
    record = wf.Latency(["a"])
    record["a"] = 99
    restored = wf.Latency.from_bytes(record.to_bytes() + b"body", ["a"])
    assert restored.stamps == [99]


def test_a_short_header_raises():
    with pytest.raises(ValueError):
        wf.Latency.from_bytes(b"\x00" * 4, ["a", "b"])


def test_from_bytes_validates_its_stage_list():
    """Deviation 5: legacy checked only the byte length."""
    with pytest.raises(ValueError):
        wf.Latency.from_bytes(b"\x00" * 16, ["a", "a"])


# ---------------------------------------------------------------------------
# TracedBytes — the carrier
# ---------------------------------------------------------------------------


def test_traced_bytes_exposes_its_payload_and_record():
    record = wf.Latency(["s"])
    message = wf.TracedBytes(b"hello", record)
    assert message.payload == b"hello"
    assert message.latency.stages == ["s"]


def test_traced_bytes_repr_includes_the_record():
    message = wf.TracedBytes(b"x", wf.Latency(["s1", "s2"]))
    assert "TracedBytes" in repr(message)
    assert "Latency(s1=0, s2=0)" in repr(message)


def test_the_record_is_shared_not_copied():
    """A stamp mutates in place, so the caller's record must be the same one."""
    record = wf.Latency(["s"])
    message = wf.TracedBytes(b"x", record)
    message.latency["s"] = 5
    assert record["s"] == 5


# ---------------------------------------------------------------------------
# stamp
# ---------------------------------------------------------------------------


def test_stamping_records_the_wall_clock_in_stage_order():
    graph, source = traced_graph(["start", "mid", "end"])
    stamped = wf.stamp(wf.stamp(wf.stamp(source, "start"), "mid"), "end")
    seen = []
    stamped.inspect(lambda message: seen.append(list(message.latency.stamps)))
    graph.run(cycles=3)

    assert len(seen) == 3
    for stamps in seen:
        assert all(s > 0 for s in stamps), stamps
        assert stamps[0] <= stamps[1] <= stamps[2], stamps


def test_stamp_precise_separates_stages_within_one_cycle():
    graph, source = traced_graph(["start", "end"], count=2)
    stamped = wf.stamp_precise(wf.stamp_precise(source, "start"), "end")
    graph.run(cycles=2)

    stamps = stamps_of(stamped)
    assert stamps[1] > stamps[0], stamps


def test_stamp_passes_the_value_through_unchanged():
    graph, source = traced_graph(["start"], count=1)
    stamped = wf.stamp(source, "start")
    graph.run(cycles=1)
    assert stamped.value().payload == b"payload"


def test_stamp_if_disabled_wires_nothing():
    graph, source = traced_graph(["start", "mid"], count=2)
    stamped = wf.stamp_precise_if(wf.stamp_if(source, "start", False), "mid", False)
    graph.run(cycles=2)
    assert stamps_of(stamped) == [0, 0]


def test_stamp_if_enabled_stamps_only_that_stage():
    graph, source = traced_graph(["start", "mid", "end"], count=2)
    stamped = wf.stamp_precise_if(wf.stamp_if(source, "start", True), "end", True)
    graph.run(cycles=2)

    stamps = stamps_of(stamped)
    assert stamps[0] > 0
    assert stamps[1] == 0  # mid was never stamped
    assert stamps[2] > 0


def test_stamping_an_unknown_stage_aborts_the_run():
    graph, source = traced_graph(["start"], count=1)
    wf.stamp(source, "nope")
    with pytest.raises(Exception, match="unknown stage 'nope'"):
        graph.run(cycles=1)


def test_stamping_a_non_traced_value_aborts_the_run():
    graph = wf.Graph()
    wf.stamp(graph.values([1.0], TICK_NANOS), "start")
    with pytest.raises(Exception, match="expected a TracedBytes"):
        graph.run(cycles=1)


def test_every_member_of_a_burst_is_stamped():
    """Deviation 3: an adapter source erases each burst to a list."""
    graph = wf.Graph()
    burst = [wf.TracedBytes(b"a", wf.Latency(["s"])), wf.TracedBytes(b"b", wf.Latency(["s"]))]
    stamped = wf.stamp(graph.values([burst], TICK_NANOS), "s")
    graph.run(cycles=1)

    assert [message.latency["s"] > 0 for message in stamped.value()] == [True, True]


# ---------------------------------------------------------------------------
# latency_report
# ---------------------------------------------------------------------------


def stamped_report(stages=("a", "b", "c"), cycles=5, **kwargs):
    """Run a fully-stamped graph through a report, returning its stats."""
    stages = list(stages)
    graph, source = traced_graph(stages, count=cycles)
    stamped = source
    for stage in stages:
        stamped = wf.stamp_precise(stamped, stage)
    _sink, stats = wf.latency_report(stamped, stages, **kwargs)
    graph.run(cycles=cycles)
    return stats


def test_a_report_counts_one_sample_per_hop_per_message():
    stats = stamped_report(cycles=5)
    assert stats.stages == ["a", "b", "c"]
    assert len(stats) == 3
    assert stats["b"]["count"] == 5
    assert stats["c"]["count"] == 5


def test_a_report_measures_real_deltas():
    stats = stamped_report(cycles=5)
    hop = stats["b"]
    assert hop["min_ns"] > 0
    assert hop["min_ns"] <= hop["mean_ns"] <= hop["max_ns"]
    assert hop["p50_ns"] > 0
    assert hop["p99_ns"] >= hop["p50_ns"]


def test_the_first_stage_has_no_hop():
    stats = stamped_report()
    with pytest.raises(KeyError):
        _ = stats["a"]


def test_an_unknown_stage_raises_key_error():
    stats = stamped_report()
    with pytest.raises(KeyError):
        _ = stats["missing"]


def test_the_report_text_names_every_hop():
    text = stamped_report().report()
    assert "latency report" in text
    assert "a -> b" in text
    assert "b -> c" in text
    assert "a -> c (end to end)" in text
    assert "never observed" not in text


def test_an_unstamped_hop_is_tallied_not_measured():
    graph, source = traced_graph(["a", "b"], count=2)
    # Only the first stage is stamped, so the a->b hop never gets a sample.
    _sink, stats = wf.latency_report(wf.stamp(source, "a"), ["a", "b"])
    graph.run(cycles=2)

    assert stats["b"]["count"] == 0
    assert stats["b"]["min_ns"] == 0
    assert stats["b"]["unstamped"] == 2
    assert "2 unstamped" in stats.report()


def test_a_same_cycle_hop_is_reported_as_unmeasured_not_as_zero():
    """Two cycle-clock stamps in one engine cycle cannot measure the hop
    between them, and saying so is the difference between "immeasurably fast"
    and "never measured"."""
    graph, source = traced_graph(["a", "b"], count=3)
    stamped = wf.stamp(wf.stamp(source, "a"), "b")
    _sink, stats = wf.latency_report(stamped, ["a", "b"], output="silent")
    graph.run(cycles=3)

    hop = stats["b"]
    assert hop["count"] == 0
    assert hop["same_instant"] == 3
    assert "3 same-cycle" in stats.report()


def test_output_stdout_prints_the_report(capfd):
    stamped_report(output="stdout")
    assert "latency report" in capfd.readouterr().out


def test_output_silent_prints_nothing(capfd):
    stamped_report(output="silent")
    assert capfd.readouterr().out == ""


def test_output_log_keeps_stdout_clean(capfd):
    """The reason `output` replaced a bare `print_on_teardown` flag: a process
    that reserves stdout for structured output could not get the report at
    all."""
    stamped_report(output="log")
    assert capfd.readouterr().out == ""


def test_an_unknown_output_is_rejected():
    graph, source = traced_graph(["a", "b"], count=1)
    with pytest.raises(ValueError):
        wf.latency_report(wf.stamp(source, "a"), ["a", "b"], output="syslog")


# ── stamp_as / stamp_all ──────────────────────────────────────────────────


def test_stamp_as_off_wires_nothing():
    graph, source = traced_graph(["a", "b"], count=1)
    stamped = wf.stamp_as(source, "a", "off")
    graph.run(cycles=1)
    assert stamps_of(stamped) == [0, 0]


def test_stamp_as_cycle_and_precise_both_stamp():
    for mode in ("cycle", "precise"):
        graph, source = traced_graph(["a", "b"], count=1)
        stamped = wf.stamp_as(source, "a", mode)
        graph.run(cycles=1)
        assert stamps_of(stamped)[0] > 0, mode


def test_an_unknown_stamping_mode_is_rejected():
    graph, source = traced_graph(["a", "b"], count=1)
    with pytest.raises(ValueError):
        wf.stamp_as(source, "a", "sometimes")


def test_stamp_all_writes_every_stage_from_one_node():
    graph, source = traced_graph(["a", "b", "c"], count=1)
    stamped = wf.stamp_all(source, ["a", "b", "c"], "precise")
    graph.run(cycles=1)

    stamps = stamps_of(stamped)
    assert all(s > 0 for s in stamps), stamps
    # A fresh read per stage, exactly as three chained stamps would give.
    # Non-strict: adjacent reads are ~20ns apart, and a monotonic clock that
    # ticks coarser than that (some ARM counters) returns equal nanoseconds.
    assert stamps[0] <= stamps[1] <= stamps[2], stamps


def test_stamp_all_under_the_cycle_clock_shares_one_snap():
    graph, source = traced_graph(["a", "b"], count=1)
    stamped = wf.stamp_all(source, ["a", "b"], "cycle")
    graph.run(cycles=1)

    stamps = stamps_of(stamped)
    assert stamps[0] > 0
    assert stamps[0] == stamps[1]


def test_stamp_all_rejects_an_empty_stage_list():
    graph, source = traced_graph(["a", "b"], count=1)
    with pytest.raises(ValueError):
        wf.stamp_all(source, [], "cycle")


# ── the read-out surface ──────────────────────────────────────────────────


def test_the_total_row_spans_the_whole_schema():
    stats = stamped_report(cycles=4)
    total = stats.total
    assert (total["from"], total["to"]) == ("a", "c")
    assert total["count"] == 4
    assert total["min_ns"] >= stats["b"]["min_ns"]


def test_a_single_stage_schema_has_no_total_to_fabricate():
    # One stage means no hop to span: mirror Rust's LatencyStats.total and
    # return all-zero numbers with empty labels, not a from == to row.
    stats = stamped_report(stages=("only",), cycles=3)
    total = stats.total
    assert (total["from"], total["to"]) == ("", "")
    assert total["count"] == 0
    # The shape is the ordinary hop dict, so callers need not branch.
    assert set(total) >= {"min_ns", "p99_ns", "same_instant"}


def test_hops_lists_every_hop_in_order_and_labels_it():
    stats = stamped_report(cycles=3)
    hops = stats.hops()
    assert [(h["from"], h["to"]) for h in hops] == [("a", "b"), ("b", "c")]
    assert all(h["count"] == 3 for h in hops)
    assert all(h["p999_ns"] >= h["p50_ns"] for h in hops)


def test_reset_drops_the_samples_and_keeps_the_stages():
    stats = stamped_report(cycles=4)
    assert stats["b"]["count"] == 4
    stats.reset()
    assert stats["b"]["count"] == 0
    assert stats.total["count"] == 0
    assert stats.stages == ["a", "b", "c"]


def test_the_sink_value_is_none():
    graph, source = traced_graph(["a", "b"], count=2)
    sink, _stats = wf.latency_report(wf.stamp(source, "a"), ["a", "b"])
    graph.run(cycles=2)
    assert sink.value() is None


def test_a_report_over_a_non_traced_stream_aborts_the_run():
    graph = wf.Graph()
    wf.latency_report(graph.values([1.0], TICK_NANOS), ["a", "b"])
    with pytest.raises(Exception, match="expected a TracedBytes"):
        graph.run(cycles=1)


def test_a_record_with_the_wrong_stage_count_aborts_the_run():
    """Deviation 6: legacy reported over whichever prefix the two shared."""
    graph, source = traced_graph(["a", "b"], count=1)
    wf.latency_report(wf.stamp(source, "a"), ["a", "b", "c"])
    with pytest.raises(Exception, match="declared 3 stages"):
        graph.run(cycles=1)


def test_an_unusable_stage_list_is_rejected_at_wiring():
    graph, source = traced_graph(["a"], count=1)
    with pytest.raises(ValueError):
        wf.latency_report(source, [])
    with pytest.raises(ValueError):
        wf.latency_report_if(source, ["a", "a"], True)


def test_latency_report_if_disabled_aggregates_nothing():
    stages = ["a", "b"]
    graph, source = traced_graph(stages, count=3)
    stamped = wf.stamp(wf.stamp(source, "a"), "b")
    sink, stats = wf.latency_report_if(stamped, stages, False, output="stdout")
    graph.run(cycles=3)

    assert stats.stages == stages
    assert stats["b"]["count"] == 0
    assert sink.value() is None


def test_a_re_run_aggregates_afresh():
    """A `counter` graph is re-runnable, so the second run must start clean."""
    stages = ["a", "b"]
    graph = wf.Graph()
    messages = graph.counter(TICK_NANOS).map(
        lambda _n: wf.TracedBytes(b"payload", wf.Latency(stages))
    )
    stamped = wf.stamp_precise(wf.stamp_precise(messages, "a"), "b")
    _sink, stats = wf.latency_report(stamped, stages, output="silent")

    graph.run(cycles=3)
    assert stats["b"]["count"] == 3
    graph.run(cycles=3)
    assert stats["b"]["count"] == 3


def test_latency_report_if_enabled_matches_latency_report():
    stages = ["a", "b"]
    graph, source = traced_graph(stages, count=3)
    stamped = wf.stamp_precise(wf.stamp_precise(source, "a"), "b")
    _sink, stats = wf.latency_report_if(stamped, stages, True, output="silent")
    graph.run(cycles=3)

    assert stats["b"]["count"] == 3

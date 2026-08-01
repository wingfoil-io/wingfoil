"""Tests for the wingfoil-next Prometheus Python bindings.

All of these run **by default** — there is no marked tier, and no external
service. The exporter *is* the server: it binds an OS-assigned port on
``serve()``, so a test can run a graph and then scrape ``/metrics`` off
127.0.0.1 with nothing but the standard library. That covers the whole path
(register → cycle → render → scrape) end to end, which is what a marked
integration tier would otherwise be for.

The Rust `prometheus_integration.rs` test is the complementary one: it checks a
real Prometheus *server* can scrape the endpoint, which needs the docker stack.
Nothing here duplicates that.
"""

import urllib.request

import pytest

import wingfoil_next as wf

MS_NANOS = 1_000_000
SECOND_NANOS = 1_000_000_000


def scrape(port):
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/metrics", timeout=5) as body:
        return body.read().decode()


def test_module_exposes_the_exporter():
    assert isinstance(wf.PrometheusExporter, type)


def test_serve_returns_the_bound_port():
    """Port 0 means "let the OS pick" — the assigned port comes back."""
    port = wf.PrometheusExporter("127.0.0.1:0").serve()
    assert port > 0


def test_serve_raises_on_a_bad_address():
    with pytest.raises(RuntimeError) as excinfo:
        wf.PrometheusExporter("not-an-address").serve()
    assert "binding metrics server" in str(excinfo.value)


def test_gauge_returns_a_terminal_stream():
    g = wf.Graph()
    exporter = wf.PrometheusExporter("127.0.0.1:0")
    sink = exporter.gauge("wingfoil_test_metric", g.constant(1.0))
    assert isinstance(sink, wf.Stream)


def test_an_empty_endpoint_scrapes_cleanly_before_any_run():
    """Registered but never cycled: the metric has no value to render yet."""
    g = wf.Graph()
    exporter = wf.PrometheusExporter("127.0.0.1:0")
    port = exporter.serve()
    exporter.gauge("wingfoil_test_unset", g.constant(1.0))

    assert "wingfoil_test_unset" not in scrape(port)


def test_a_gauge_publishes_its_latest_value():
    g = wf.Graph()
    exporter = wf.PrometheusExporter("127.0.0.1:0")
    port = exporter.serve()
    exporter.gauge("wingfoil_test_counter", g.counter(period_nanos=50 * MS_NANOS))

    g.run(realtime=True, duration_nanos=300 * MS_NANOS)

    body = scrape(port)
    assert "wingfoil_test_counter" in body
    # The rendered value is the counter's last tick — at least a few by 300ms.
    value = next(
        line for line in body.splitlines() if line.startswith("wingfoil_test_counter ")
    )
    assert float(value.split()[1]) >= 1


def test_several_gauges_share_one_exporter():
    g = wf.Graph()
    exporter = wf.PrometheusExporter("127.0.0.1:0")
    port = exporter.serve()
    ticks = g.counter(period_nanos=50 * MS_NANOS)
    exporter.gauge("wingfoil_test_a", ticks)
    exporter.gauge("wingfoil_test_b", ticks.map(lambda v: v * 2))

    g.run(realtime=True, duration_nanos=300 * MS_NANOS)

    body = scrape(port)
    assert "wingfoil_test_a" in body and "wingfoil_test_b" in body


def test_a_historical_run_publishes_nothing():
    """A backtest must not push fast-forwarded values to a live endpoint."""
    g = wf.Graph()
    exporter = wf.PrometheusExporter("127.0.0.1:0")
    port = exporter.serve()
    exporter.gauge("wingfoil_test_historical", g.counter(period_nanos=SECOND_NANOS))

    g.run(start_nanos=0, duration_nanos=5 * SECOND_NANOS)

    assert "wingfoil_test_historical" not in scrape(port)


def test_a_raising_str_aborts_the_run():
    """Legacy published an empty string here; this fails loudly instead."""

    class Bad:
        def __str__(self):
            raise ValueError("nope")

    g = wf.Graph()
    exporter = wf.PrometheusExporter("127.0.0.1:0")
    exporter.serve()
    exporter.gauge("wingfoil_test_bad", g.constant(Bad()))

    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=True, duration_nanos=100 * MS_NANOS)
    assert "str() on the stream value raised" in str(excinfo.value)

"""
Latency measurement example using wingfoil's Latency/TracedBytes classes.

Demonstrates:
  - Declaring a latency schema with Latency(stages=[...])
  - Wrapping payloads in TracedBytes
  - Stamping stages with .stamp("stage_name")
  - Printing per-stage delta stats with .latency_report()

Run:
  cd wingfoil-python && maturin develop --features iceoryx2-beta && python examples/latency.py
"""

from wingfoil import ticker, Latency, TracedBytes, Graph, StampMode, StampModeHandle

STAGES = ["produce", "encode", "decode", "strategy", "ack"]


def make_payload(seq):
    """Wrap a sequence number in TracedBytes with an empty latency record."""
    return TracedBytes(f"quote-{seq}".encode(), Latency(STAGES))


def main():
    # Share one handle across every stamp call; flip stamping on/off (or
    # toggle to precise) at runtime via handle.set(...).
    mode = StampModeHandle(StampMode.On)

    pipeline = (
        ticker(0.01)
        .count()
        .map(make_payload)
        .stamp("produce", mode)
        .stamp("encode", mode)
        # ── pretend strategy work happens here ──
        .map(lambda t: t)  # identity (placeholder for real logic)
        .stamp("decode", mode)
        .stamp("strategy", StampMode.OnPrecise)
        .stamp("ack", mode)
    )

    report = pipeline.latency_report_if(STAGES, True, print_on_teardown=True)

    print("Running latency pipeline for 0.5 seconds...")
    Graph([report]).run(realtime=True, duration=0.5)
    print("Done.")


if __name__ == "__main__":
    main()

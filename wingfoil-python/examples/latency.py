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

from wingfoil import ticker, Latency, TracedBytes, Graph

STAGES = ["produce", "encode", "decode", "strategy", "ack"]


def make_payload(seq):
    """Wrap a sequence number in TracedBytes with an empty latency record."""
    return TracedBytes(f"quote-{seq}".encode(), Latency(STAGES))


def main():
    stamp = True  # flip to False to disable all stamping (zero cost)

    pipeline = (
        ticker(0.01)
        .count()
        .map(make_payload)
        .stamp_if("produce", stamp)
        .stamp_if("encode", stamp)
        # ── pretend strategy work happens here ──
        .map(lambda t: t)  # identity (placeholder for real logic)
        .stamp_if("decode", stamp)
        .stamp_precise_if("strategy", stamp)
        .stamp_if("ack", stamp)
    )

    report = pipeline.latency_report_if(STAGES, stamp, print_on_teardown=True)

    print("Running latency pipeline for 0.5 seconds...")
    Graph([report]).run(realtime=True, duration=0.5)
    print("Done.")


if __name__ == "__main__":
    main()

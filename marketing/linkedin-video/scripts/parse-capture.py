"""Turn captured `top_of_book` transcripts into the JSON the video renders."""

import argparse
import json
import re
import statistics

# "<engine time>  bid  585.33  ask  585.91  spread  0.58  mid  585.62"
QUOTE = re.compile(
    r"^(?P<time>[\d,]+\.[\d_]+)\s+"
    r"bid\s+(?P<bid>[\d.]+)\s+ask\s+(?P<ask>[\d.]+)\s+"
    r"spread\s+(?P<spread>[\d.]+)\s+mid\s+(?P<mid>[\d.]+)$"
)
SUMMARY = re.compile(r"^(?P<quotes>\d+) quote changes$")
# "3.0s of market data replayed in 797.540µs — 3762× faster than real time"
REPLAY = re.compile(
    r"^(?P<market>[\d.]+)s of market data replayed in "
    r"(?P<elapsed>[\d.]+)(?P<unit>ms|µs|s) — [\d,]+× faster than real time$"
)
PACED = re.compile(r"^[\d.]+m?s elapsed, paced by the feed$")

TO_MS = {"s": 1000.0, "ms": 1.0, "µs": 0.001}

# How many rows the video's terminal shows.
ROWS = 6


def parse(path):
    """Rows, quote count, and replay milliseconds (None for a live run)."""
    rows, quotes, replay_ms = [], None, None
    for line in open(path):
        line = line.strip()
        if not line or PACED.match(line):
            continue
        if m := SUMMARY.match(line):
            quotes = int(m.group("quotes"))
            continue
        if m := REPLAY.match(line):
            replay_ms = float(m.group("elapsed")) * TO_MS[m.group("unit")]
            continue
        m = QUOTE.match(line)
        if not m:
            raise SystemExit(f"unparsed line in {path}: {line!r}")
        rows.append(
            {
                "time": m.group("time"),
                "bid": m.group("bid"),
                "ask": m.group("ask"),
                "spread": m.group("spread"),
                "mid": m.group("mid"),
            }
        )
    if not rows:
        raise SystemExit(f"no quotes captured in {path}")
    if quotes is None:
        raise SystemExit(f"no summary line in {path} — did the run finish?")
    return rows, quotes, replay_ms


def quote_of(row):
    """The half of a row that must not change between run modes."""
    return (row["bid"], row["ask"], row["spread"], row["mid"])


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--historical", required=True, nargs="+")
    ap.add_argument("--realtime", required=True)
    ap.add_argument("--command", required=True)
    ap.add_argument("--market-seconds", type=float, default=3.0)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    runs = [parse(p) for p in args.historical]
    hist, hist_quotes, _ = runs[0]
    live, live_quotes, _ = parse(args.realtime)

    # Every repeat must agree with the first, or the replay is not deterministic
    # and the video's whole premise is wrong.
    for path, (rows, quotes, _) in zip(args.historical[1:], runs[1:]):
        if [quote_of(r) for r in rows] != [quote_of(r) for r in hist] or quotes != hist_quotes:
            raise SystemExit(f"{path}: a historical replay differed from the first — not deterministic")

    # The video's central claim: one graph definition, identical quotes in
    # identical order, whichever way it is run. Fail the build if that breaks.
    if [quote_of(r) for r in hist] != [quote_of(r) for r in live]:
        raise SystemExit(
            "quotes differ across run modes — the video's claim no longer holds.\n"
            "A live run reads the book at cycle boundaries, so under heavy load it "
            "can coalesce updates a replay resolves separately. Re-run on a quieter "
            "machine before changing the video's wording."
        )

    # The engine clocks, in contrast, are *expected* to differ.
    if hist[0]["time"] == live[0]["time"]:
        raise SystemExit("both captures share a start time — did the realtime run actually run?")

    timings = sorted(r[2] for r in runs if r[2] is not None)
    if len(timings) != len(runs):
        raise SystemExit("a historical run printed no replay timing")
    median_ms = statistics.median(timings)

    json.dump(
        {
            "note": (
                "Captured by scripts/capture-output.sh from real --release runs of "
                "crates/wingfoil/examples/core/top_of_book, over the LOBSTER AAPL "
                "sample. Nothing is reformatted. Replay timing is the median of "
                f"{len(timings)} runs; absolute times are machine-specific."
            ),
            "command": args.command,
            "quotes": hist_quotes,
            "rows": ROWS,
            "replay": {
                "marketSeconds": args.market_seconds,
                "medianMs": round(median_ms, 3),
                "fastestMs": round(timings[0], 3),
                "slowestMs": round(timings[-1], 3),
                "runs": len(timings),
                "speedup": round(args.market_seconds * 1000 / median_ms),
            },
            "historical": hist[:ROWS],
            "realtime": live[:ROWS],
        },
        open(args.out, "w"),
        indent=2,
    )
    print(
        f"wrote {args.out}: {hist_quotes} quotes per mode, first {ROWS} rendered; "
        f"replay median {median_ms:.3f} ms of {len(timings)} runs "
        f"({timings[0]:.3f}–{timings[-1]:.3f})",
        flush=True,
    )


if __name__ == "__main__":
    main()

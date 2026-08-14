"""Turn two captured `top_of_book` transcripts into the JSON the video renders."""

import argparse
import json
import re

# "<engine time>  bid  585.33  ask  585.91  spread  0.58  mid  585.62"
QUOTE = re.compile(
    r"^(?P<time>[\d,]+\.[\d_]+)\s+"
    r"bid\s+(?P<bid>[\d.]+)\s+ask\s+(?P<ask>[\d.]+)\s+"
    r"spread\s+(?P<spread>[\d.]+)\s+mid\s+(?P<mid>[\d.]+)$"
)
SUMMARY = re.compile(r"^\d+ quote changes$")

# How many rows the video's terminal shows.
ROWS = 6


def parse(path):
    rows, summary = [], None
    for line in open(path):
        line = line.strip()
        if not line:
            continue
        if SUMMARY.match(line):
            summary = line
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
    if summary is None:
        raise SystemExit(f"no summary line in {path} — did the run finish?")
    return rows, summary


def quote_of(row):
    """The half of a row that must not change between run modes."""
    return (row["bid"], row["ask"], row["spread"], row["mid"])


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--historical", required=True)
    ap.add_argument("--realtime", required=True)
    ap.add_argument("--command", required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    hist, hist_summary = parse(args.historical)
    live, live_summary = parse(args.realtime)

    # The video's entire claim: one graph definition, identical quotes in
    # identical order, whichever way it is run. Fail the build if that breaks.
    if [quote_of(r) for r in hist] != [quote_of(r) for r in live]:
        raise SystemExit(
            "quotes differ across run modes — the video's claim no longer holds.\n"
            "A live run reads the book at cycle boundaries, so under heavy load it "
            "can coalesce updates a replay resolves separately. Re-run on a quieter "
            "machine before changing the video's wording."
        )

    # The engine clocks, in contrast, are *expected* to differ: a replay starts
    # at the first message, a live run reads the wall clock. Assert that too, so
    # a capture that quietly recorded the same mode twice cannot slip through.
    if hist[0]["time"] == live[0]["time"]:
        raise SystemExit("both captures share a start time — did the realtime run actually run?")

    json.dump(
        {
            "note": (
                "Captured by scripts/capture-output.sh from real runs of "
                "crates/wingfoil/examples/core/top_of_book, over the LOBSTER "
                "AAPL sample. Nothing is reformatted."
            ),
            "command": args.command,
            "summary": hist_summary,
            "rows": ROWS,
            "historical": hist[:ROWS],
            "realtime": live[:ROWS],
        },
        open(args.out, "w"),
        indent=2,
    )
    print(
        f"wrote {args.out}: {len(hist)} quotes captured per mode, "
        f"first {ROWS} rendered ({hist_summary})",
        flush=True,
    )


if __name__ == "__main__":
    main()

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
# "91997 messages, 15387 quote changes"
SUMMARY = re.compile(r"^(?P<messages>\d+) messages, (?P<quotes>\d+) quote changes$")
# "replayed in 166.542ms — 552395 messages/sec"
REPLAY = re.compile(
    r"^replayed in (?P<elapsed>[\d.]+)(?P<unit>ms|µs|s) — (?P<rate>\d+) messages/sec$"
)
PACED = re.compile(r"^[\d.]+m?s elapsed, paced by the feed$")

TO_MS = {"s": 1000.0, "ms": 1.0, "µs": 0.001}

# How many rows the video's terminal shows.
ROWS = 6


def parse(path):
    """Rows, counts, and replay milliseconds (None for a live run)."""
    rows, quotes, messages, replay_ms, rate = [], None, None, None, None
    for line in open(path):
        line = line.strip()
        if not line or PACED.match(line):
            continue
        if m := SUMMARY.match(line):
            quotes, messages = int(m.group("quotes")), int(m.group("messages"))
            continue
        if m := REPLAY.match(line):
            replay_ms = float(m.group("elapsed")) * TO_MS[m.group("unit")]
            rate = int(m.group("rate"))
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
    return rows, quotes, messages, replay_ms, rate


def quote_of(row):
    """The half of a row that must not change between run modes."""
    return (row["bid"], row["ask"], row["spread"], row["mid"])


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--historical", required=True, nargs="+")
    ap.add_argument("--realtime", required=True)
    ap.add_argument("--command", required=True)
    ap.add_argument("--market-hours", type=float, default=1.0)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    runs = [parse(p) for p in args.historical]
    hist, hist_quotes, hist_messages, _, _ = runs[0]
    live, live_quotes, live_messages, _, _ = parse(args.realtime)

    # Every repeat must agree with the first, or the replay is not deterministic
    # and the video's whole premise is wrong.
    for path, (rows, quotes, _, _, _) in zip(args.historical[1:], runs[1:]):
        if [quote_of(r) for r in rows] != [quote_of(r) for r in hist] or quotes != hist_quotes:
            raise SystemExit(f"{path}: a historical replay differed from the first — not deterministic")

    # The video's central claim: one graph definition, identical quotes in
    # identical order, whichever way it is run. The live feed is a *window* of
    # the replay -- an hour cannot be delivered live in an hour of CI -- so the
    # comparison is against the replay's matching prefix.
    if [quote_of(r) for r in hist[: len(live)]] != [quote_of(r) for r in live]:
        raise SystemExit(
            "quotes differ across run modes — the video's claim no longer holds.\n"
            "A live run reads the book at cycle boundaries, so under heavy load it "
            "can coalesce updates a replay resolves separately. Re-run on a quieter "
            "machine before changing the video's wording."
        )
    if len(live) >= len(hist):
        raise SystemExit("the live window is not a strict prefix of the replay — check the feed split")

    # The engine clocks, in contrast, are *expected* to differ.
    if hist[0]["time"] == live[0]["time"]:
        raise SystemExit("both captures share a start time — did the realtime run actually run?")

    timings = sorted(r[3] for r in runs if r[3] is not None)
    if len(timings) != len(runs):
        raise SystemExit("a historical run printed no replay timing")
    median_ms = statistics.median(timings)
    rate = round(hist_messages / (median_ms / 1000))

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
                "marketHours": args.market_hours,
                "messages": hist_messages,
                "quotes": hist_quotes,
                "medianMs": round(median_ms, 1),
                "fastestMs": round(timings[0], 1),
                "slowestMs": round(timings[-1], 1),
                "runs": len(timings),
                "messagesPerSecond": rate,
            },
            "live": {"messages": live_messages, "quotes": live_quotes},
            "historical": hist[:ROWS],
            "realtime": live[:ROWS],
        },
        open(args.out, "w"),
        indent=2,
    )
    print(
        f"wrote {args.out}: replay {hist_messages:,} messages -> {hist_quotes:,} quotes "
        f"in {median_ms:.1f} ms median of {len(timings)} runs "
        f"({timings[0]:.1f}–{timings[-1]:.1f}), {rate:,} msg/s; "
        f"live window {live_messages:,} messages -> {live_quotes} quotes",
        flush=True,
    )


if __name__ == "__main__":
    main()

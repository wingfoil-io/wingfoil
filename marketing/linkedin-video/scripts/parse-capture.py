"""Turn two captured `logged` transcripts into the JSON the video renders."""

import argparse
import json
import re

LINE = re.compile(r'^(\S+)\s+(\S+)\s+"(.*)"$')


def parse(path):
    rows = []
    for line in open(path):
        line = line.strip()
        if not line:
            continue
        m = LINE.match(line)
        if not m:
            raise SystemExit(f"unparsed log line in {path}: {line!r}")
        rows.append({"time": m.group(1), "label": m.group(2), "value": m.group(3)})
    if not rows:
        raise SystemExit(f"no log lines captured in {path} — was RUST_LOG=info set?")
    return rows


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--historical", required=True)
    ap.add_argument("--realtime", required=True)
    ap.add_argument("--command", required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    hist, live = parse(args.historical), parse(args.realtime)

    # The video's entire claim: one graph definition, identical values in
    # identical order, whichever way it is run. Fail the build if that breaks.
    if [r["value"] for r in hist] != [r["value"] for r in live]:
        raise SystemExit("values differ across run modes — the video's claim no longer holds")

    # The engine clocks, in contrast, are *expected* to differ: a replay starts
    # at NanoTime::ZERO, a live run reads the wall clock. Assert that too, so a
    # capture that quietly recorded the same mode twice cannot slip through.
    if hist[0]["time"] == live[0]["time"]:
        raise SystemExit("both captures share a start time — did the realtime run actually run?")

    json.dump(
        {
            "note": (
                "Captured by scripts/capture-output.sh from real runs of "
                "crates/wingfoil/examples/core/odds_evens. Only env_logger's "
                "volatile wall-clock prefix is stripped."
            ),
            "command": args.command,
            "historical": hist,
            "realtime": live,
        },
        open(args.out, "w"),
        indent=2,
    )
    print(
        f"wrote {args.out}: {len(hist)} historical + {len(live)} realtime rows",
        flush=True,
    )


if __name__ == "__main__":
    main()

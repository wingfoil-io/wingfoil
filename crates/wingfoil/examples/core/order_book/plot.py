#!/usr/bin/env python3
"""Plot the order_book example's two output streams.

Run the example first — it writes `data/prices.csv` and `data/fills.csv`
alongside its input:

    cargo run --release -p wingfoil \
        --features csv --example order_book
    pip install pandas matplotlib
    python3 crates/wingfoil/examples/core/order_book/plot.py

Writes `aapl.svg` next to this script (pass `--show` to open a window instead).
"""

import sys
from datetime import datetime, timezone
from pathlib import Path

import matplotlib.dates as mdates
import matplotlib.pyplot as plt
import pandas as pd

HERE = Path(__file__).parent
DATA = HERE / "data"
# LOBSTER quotes prices in units of 1/10000 of a dollar.
PRICE_SCALE = 1e-5


def load(name):
    df = pd.read_csv(DATA / name)
    df["datetime"] = df["time"].apply(
        lambda t: datetime.fromtimestamp(t * 1e-9, tz=timezone.utc)
    )
    return df


def main():
    prices, trades = load("prices.csv"), load("fills.csv")

    trades["price"] *= PRICE_SCALE
    prices["bid_price"] *= PRICE_SCALE
    prices["ask_price"] *= PRICE_SCALE

    lifted = trades[trades["side"] == "LiftAsk"]
    hit = trades[trades["side"] == "HitBid"]

    _fig, ax = plt.subplots(1)

    # Two-way prices are a step function — they hold until the book changes.
    ax.plot(prices["datetime"], prices["bid_price"], drawstyle="steps-post",
            color="green", alpha=0.5, label="best bid")
    ax.plot(prices["datetime"], prices["ask_price"], drawstyle="steps-post",
            color="red", alpha=0.5, label="best ask")

    # Fills are point events, sized by quantity — a third, sparser frequency.
    ax.scatter(lifted["datetime"], lifted["price"], lifted["quantity"],
               color="green", marker="^", alpha=0.2, label="Ask Lifted")
    ax.scatter(hit["datetime"], hit["price"], hit["quantity"],
               color="red", marker="v", alpha=0.2, label="Bid Hit")

    ax.xaxis.set_major_formatter(mdates.DateFormatter("%H:%M:%S"))
    plt.xlabel("Time")
    plt.ylabel("AAPL Price")

    legend = plt.legend()
    for handle in legend.legend_handles[2:]:
        handle.set_sizes([20])

    if "--show" in sys.argv:
        plt.show()
    else:
        out = HERE / "aapl.svg"
        plt.savefig(out)
        print(f"wrote {out}")


if __name__ == "__main__":
    main()

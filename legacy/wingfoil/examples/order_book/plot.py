#!/usr/bin/env python3


"""
routine to plot fills and prices csv data
"""

import pandas as pd
import matplotlib.pyplot as plt
from datetime import datetime
import matplotlib.dates as mdates


prices = pd.read_csv('~/wingfoil/src/examples/lobster/data/prices.csv')
trades = pd.read_csv('~/wingfoil/src/examples/lobster/data/fills.csv')

for df in prices, trades:
    df["datetime"] = df["time"].apply(lambda t: datetime.utcfromtimestamp(t*1e-9))


price_scale = 1e-5
trades["price"] *= price_scale
prices["bid_price"] *= price_scale
prices["ask_price"] *= price_scale


lifted = trades[trades['side'] == 'LiftAsk']
hit = trades[trades['side'] == 'HitBid']

plt.clf()

#trades['direction'] = trades['side'].apply(lambda s: 1 if s == 'HitBid' else -1)
trades['signed_quantity'] = trades.apply(lambda row: row.quantity * (-1 if row.side == 'HitBid' else 1), axis=1)
trades['net'] = trades['signed_quantity'].cumsum()

print('prices')
print(prices)
print('trades')
print(trades)
print('hit')
print(hit)
print('lifted')
print(lifted)

fig, ax1 = plt.subplots(1)

ax1.plot(prices["datetime"], prices["bid_price"], drawstyle="steps-post", color='green', alpha = 0.5, label = "best bid")
ax1.plot(prices["datetime"], prices["ask_price"], drawstyle="steps-post", color='red', alpha = 0.5, label = "best ask")

scale = 1.0
ax1.scatter(lifted["datetime"], lifted["price"], lifted["quantity"] * scale, color = 'green', marker = "^", alpha = 0.2, label = "Ask Lifted")
ax1.scatter(hit["datetime"],    hit["price"],    hit["quantity"]    * scale, color = 'red',   marker = "v", alpha = 0.2, label = "Bid Hit")

ax1.xaxis.set_major_formatter(mdates.DateFormatter('%H:%M:%S'))
plt.xlabel("Time")
plt.ylabel("AAPL Price")

legend = plt.legend()
legend.legendHandles[2].set_sizes([20])
legend.legendHandles[3].set_sizes([20])

plt.show()

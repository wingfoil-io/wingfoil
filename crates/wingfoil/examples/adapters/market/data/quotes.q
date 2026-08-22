/ Synthetic EUR/USD top-of-book history for the market_adapter example's
/ historical mode: a fixed walk (no RNG), one quote every 1.5 seconds for a
/ minute from 2000.01.01D00:00:00 — so every load serves identical rows and
/ the example's replay output is exactly reproducible.
/
/ Prices are integer pipettes (1e-5) and sizes whole units, matching what the
/ example's `QuoteRow` decodes — exact integers end to end, no floats.
/
/ Load into a fresh q session listening on port 5000:
/
/   q crates/wingfoil/examples/adapters/market/data/quotes.q -p 5000

n:40
step:0D00:00:01.500000000
mids:109312+sums n#-1 1 1 0 -1 1 -1 -1 0 1 1 -1 1 0 -1 1
sprd:1+(til n) mod 3
quotes:([]
  time:2000.01.01D00:00:00.000000000+step*til n;
  bid:mids-sprd;
  bidQty:100000*1+(til n) mod 5;
  ask:mids+sprd;
  askQty:100000*3-(til n) mod 3)

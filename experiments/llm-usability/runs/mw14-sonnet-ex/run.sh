#!/bin/sh
# full backtest, run from the workspace (about 2-3 minutes)
bin/abt run --strategy mw14 --bundle ~/data/abt/b_mw14 --untested \
  --price-relation trclose --actions in-prices \
  --cash 1000000 --compounding off --lot fractional \
  --commission-bps 10 --commission 0 --commission-min 0 --fee-bps 0 \
  --slippage-bps 0 --slippage-vol 0 --impact 0 --participation 0 \
  --cash-rate 0 --margin-rate 0 \
  --delist-proceeds last-price --on-leverage allow \
  --report-by bar --periods-per-year 52 \
  --start 1991-01-01 --returns mw14_returns.parquet env lib final.dsl

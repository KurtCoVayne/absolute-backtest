#!/bin/sh
# Full MW14 backtest under the run conventions, from the workspace.
cd "$(dirname "$0")" || exit 1
bin/abt run --strategy mw14 --data data/weekly \
  --capital 1000000 --compounding off --lot fractional \
  --price-relation trclose --actions in-prices \
  --frictionless --commission-bps 10 --margin-rate 0 \
  --on-leverage allow --delist-proceeds last-price \
  --start 2016-01-01 --periods-per-year 52 \
  env lib final.dsl

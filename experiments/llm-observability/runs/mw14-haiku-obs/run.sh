#!/usr/bin/env bash
# MW14 full backtest, run from the workspace (the directory this script sits in).
cd "$(dirname "$0")" && bin/abt run --strategy mw14 --data data/weekly \
  --capital 1000000 --compounding off \
  --frictionless --commission-bps 10 --margin-rate 0 \
  --on-leverage allow --delist-proceeds last-price \
  --price-relation trclose --actions in-prices --lot fractional \
  --start 2016-01-01 --periods-per-year 52 \
  env lib final.dsl

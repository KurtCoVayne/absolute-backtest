#!/bin/sh

bin/abt run --strategy mw14 --bundle ~/data/abt/b_mw14 --untested \
  --price-relation trclose --actions in-prices --lot fractional --cash 1000000 --compounding off \
  --commission 0 --commission-min 0 --commission-bps 10 --fee-bps 0 --slippage-bps 0 --slippage-vol 0 \
  --impact 0 --participation 0 --delist-proceeds last-price --on-leverage allow --margin-rate 0 --cash-rate 0 \
  --report-by bar --periods-per-year 52 --start 1991-01-01 --returns returns.csv "$@" env lib final.dsl

#!/bin/sh
# run from the workspace
bin/abt run --strategy r8l --bundle ~/data/abt/b_r8l_fb \
  --cash 1000000 --compounding off --lot fractional --price-relation close_m \
  --slippage-bps 0 --slippage-vol 0 --impact 0 --participation 0 --fee-bps 0 \
  --on-leverage allow --margin none --on-margin-call allow --on-ruin continue --margin-rate 0 --cash-rate 0 --short-rebate 0 \
  --report-by day --report-calendar calendar --periods-per-year 252 \
  --returns out/returns.csv --dump out/dump --timing env lib final.dsl

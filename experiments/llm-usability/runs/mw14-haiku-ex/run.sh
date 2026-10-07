#!/bin/bash
# MW14 Weekly Momentum Strategy - Full Backtest
# Runs from 1991-01-01 to 2026-07-31 with weekly returns output

bin/abt run \
  --strategy mw14 \
  --start 1991-01-01 \
  --end 2026-07-31 \
  --untested \
  --bundle ~/data/abt/b_mw14 \
  --returns returns.parquet \
  --periods-per-year 52 \
  env lib final.dsl

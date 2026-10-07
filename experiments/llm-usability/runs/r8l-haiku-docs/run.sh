#!/bin/bash
# R8L intraday futures opening-range book backtest
# Run from the workspace directory

bin/abt run --strategy r8l --data ~/data/abt/b_r8l_fb \
  --cash 1000000 \
  --lot fractional \
  --report-by day \
  --periods-per-year 252 \
  --returns results/returns.parquet \
  --dump results/ \
  env lib final.dsl

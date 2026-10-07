#!/bin/bash
# R8L intraday futures opening-range book strategy
# Full backtest with 183K bars from 2000-2026, 26 futures roots

cd "$(dirname "$0")"

bin/abt run \
  --strategy r8l \
  --bundle ~/data/abt/b_r8l_fb \
  --returns results_returns.csv \
  --dump results_records.csv \
  env lib final.dsl

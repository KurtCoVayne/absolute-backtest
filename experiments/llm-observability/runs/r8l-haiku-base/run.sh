#!/bin/bash
# Full R8L backtest under the TASK.md run conventions; run from the workspace.
cd "$(dirname "$0")" || exit 1
bin/abt run --strategy r8l --data data/sessions \
  --capital 1000000 --compounding off --lot fractional \
  --price-relation close_m \
  --frictionless --margin-rate 0 --on-leverage allow --on-margin-call allow \
  --report-by day --report-calendar calendar --periods-per-year 252 \
  env lib final.dsl

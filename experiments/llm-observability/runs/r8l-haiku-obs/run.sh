#!/bin/sh
# Full R8L backtest under the run conventions in TASK.md. Run from the workspace.
cd "$(dirname "$0")" || exit 1
bin/abt run --strategy r8l --data data/sessions \
  --capital 1000000 --compounding off --lot fractional \
  --frictionless --margin-rate 0 --on-leverage allow --on-margin-call allow \
  --price-relation close_m \
  --report-by day --report-calendar calendar --periods-per-year 252 \
  env lib final.dsl

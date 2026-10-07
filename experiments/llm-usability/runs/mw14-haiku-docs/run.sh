#!/bin/bash

# Full backtest run for MW14 strategy
# Run from the workspace directory

bin/abt run --strategy mw14 --returns returns.csv --untested \
  ~/data/abt/b_mw14 env lib final.dsl

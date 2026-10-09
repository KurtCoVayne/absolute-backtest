#!/usr/bin/env bash
# Full backtest of momentum_liquid_monthly on the seeded synthetic market (TASK.md).
# --compounding on: target weights are fractions of current equity, so each of the
# ten names is 10% of the book at every rebalance (equal weight, see NOTES.md).
cd "$(dirname "$0")" && bin/abt run --strategy momentum_liquid_monthly --synthetic --days 1000 --symbols A,B,C,D,E,F,G,H,I,J,K,L,M,N,O --compounding on env lib final.dsl

#!/usr/bin/env bash
# Full backtest on the seeded synthetic market, per the TASK.md run convention.
# Run from the workspace directory (the directory containing bin/, env/, lib/).
bin/abt run --strategy momentum_liquid_monthly --synthetic --days 1000 --symbols A,B,C,D,E,F,G,H,I,J,K,L,M,N,O env lib final.dsl

#!/bin/sh
# Full backtest of final.dsl on the seeded synthetic market (TASK.md run conventions), from the workspace.
cd "$(dirname "$0")" && bin/abt run --strategy breakout_with_stop --synthetic --days 1000 --symbols A,B,C,D,E,F,G,H,I,J,K,L env lib final.dsl

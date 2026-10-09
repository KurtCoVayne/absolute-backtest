#!/usr/bin/env bash
# Full backtest of breakout_with_stop on the seeded synthetic market, per TASK.md.
cd "$(dirname "$0")" && bin/abt run --strategy breakout_with_stop --synthetic --days 1000 --symbols A,B,C,D,E,F,G,H,I,J,K,L env lib final.dsl

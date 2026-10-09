#!/usr/bin/env bash
# Full backtest on the seeded synthetic market, per TASK.md run conventions.
cd "$(dirname "$0")" && bin/abt run --strategy vol_targeted_trend --synthetic --days 1000 --symbols SPY env lib final.dsl

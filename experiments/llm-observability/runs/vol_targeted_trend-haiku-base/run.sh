#!/usr/bin/env bash
# Full backtest under the TASK.md run conventions: seeded synthetic market, 1000 days, SPY.
# Default accounting (fixed base). Run from the workspace; the script changes into its own directory.
# Not the convention, reported in NOTES.md: adding --compounding on sizes the weights against equity.
cd "$(dirname "$0")" && bin/abt run --strategy vol_targeted_trend --synthetic --days 1000 --symbols SPY env lib final.dsl

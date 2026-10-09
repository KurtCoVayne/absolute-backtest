#!/usr/bin/env bash
# Full backtest of final.dsl on the seeded synthetic market: 40 sessions of 390 minute bars.
set -euo pipefail
cd "$(dirname "$0")"
bin/abt run --strategy intraday_open_gap --synthetic --days 40 env lib final.dsl

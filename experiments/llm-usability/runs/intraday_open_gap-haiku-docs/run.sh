#!/bin/bash
# Run the intraday_open_gap strategy on synthetic data
./bin/abt run --strategy intraday_open_gap --synthetic --days 30 --symbols SPY --seed 42 env lib final.dsl

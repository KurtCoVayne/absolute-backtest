#!/bin/bash
# Run the momentum_liquid_monthly strategy on synthetic data
# Run from the workspace directory

./bin/abt run --strategy momentum_liquid_monthly \
  --synthetic --days 1000 --symbols "A,B,C,D,E,F,G,H,I,J,K,L,M,N,O" --seed 42 \
  env lib final.dsl

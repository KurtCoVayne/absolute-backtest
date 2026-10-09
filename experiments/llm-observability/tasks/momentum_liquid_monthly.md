# Task: brief momentum_liquid_monthly

A cross-sectional momentum strategy on liquid US equities: at each month
start, hold the ten index members with the strongest return over the past
twelve months skipping the most recent month, equally weighted; names must
have at least a million dollars of average daily volume. Rebalance monthly;
everything else flat. Use the catalog environment and total returns.

Data: there is no data directory for this task. Run on the seeded synthetic market (`bin/abt run --strategy NAME --synthetic --days 1000 --symbols A,B,C,D,E,F,G,H,I,J,K,L,M,N,O ...`, see `bin/abt` usage).

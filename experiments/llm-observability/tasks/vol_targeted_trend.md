# Task: brief vol_targeted_trend

A trend-following strategy on a single index tracker: long when its price is
above its 200-day moving average, flat otherwise, sized so that the
position's realized volatility is about ten percent a year, with the weight
capped at one. Daily decisions.

Data: there is no data directory for this task. Run on the seeded synthetic market (`bin/abt run --strategy NAME --synthetic --days 1000 --symbols SPY ...`, see `bin/abt` usage).

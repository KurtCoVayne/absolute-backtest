# Task: brief breakout_with_stop

A breakout strategy: buy a name when its close is a new 52-week high with
volume above twice its 20-day average, and sell when the close falls more
than eight percent from the highest close since entry. One position per
name, at most ten names, equal size.

Data: there is no data directory for this task. Run on the seeded synthetic market (`bin/abt run --strategy NAME --synthetic --days 1000 --symbols A,B,C,D,E,F,G,H,I,J,K,L ...`, see `bin/abt` usage).

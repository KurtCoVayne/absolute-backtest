# Task: brief intraday_open_gap

An opening-gap strategy on minute bars: if the first minute's close of the
day is more than one percent above the previous day's close, buy at the
second minute and sell at the day's last minute; otherwise do nothing.

Data: there is no data directory for this task. Run on the seeded synthetic market (`bin/abt run --strategy NAME --synthetic --days 40 ...`, see `bin/abt` usage; the synthetic minute market has 390 bars a day).

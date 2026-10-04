# Minute-bar environment. Relations at @1m are named differently from their
# daily counterparts: `close_m` @1m and `close` @1d are different relations.
environment equities_1m {
  close_m(+A: Equity, @T: Timestamp, -P: Price<USD>) @1m
  volume_m(+A: Equity, @T: Timestamp, -V: Quantity<Shares>) @1m
  universe_m(-A: Equity, @T: Timestamp) @1m complete
}

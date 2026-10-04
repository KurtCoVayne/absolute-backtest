# Buy a close above the prior-year high, with a cooldown against re-entry;
# time-based exit through the kernel's own decision history.
strategy breakout_52w {
  env equities_1d
  uses features
  resolution @1d
  mode delta

  param lookback : Duration = 1y in 6mo..2y
  param lookback_min : Count = 200
  param hold : Duration = 20d in 5d..60d
  param cooldown : Duration = 60d
  param qty : Quantity<Shares> = 100 shares

  rel breakout(-A: Equity, @T: Timestamp)
  breakout(A, T) :- universe(A, T), close(A, T, P), highest(A, T, lookback, lookback_min, H), P > H.

  decide(T, buy(A, qty)) :- breakout(A, T), flat(A, T), not bought_within(A, T, cooldown).
  decide(T, sell(A, Q)) :- held(A, T, Q), lag(T, hold, T0), decided(T0, buy(A, _)).
}

# Buy a volume spike on an up day; hold for a fixed number of days.
strategy volume_spike {
  env equities_1d
  uses features
  resolution @1d
  mode delta

  param lb : Duration = 20d
  param lb_min : Count = 15
  param mult : Scalar = 2.0
  param min_ret : Scalar = 0.02
  param hold : Duration = 5d
  param qty : Quantity<Shares> = 100 shares

  rel spike(-A: Equity, @T: Timestamp)
  spike(A, T) :- universe(A, T), volume(A, T, V),
      AvgV = mean(V1) over (T1 in prior_window(T, lb, min lb_min), volume(A, T1, V1)),
      V > AvgV * mult, logret(A, T, R), R > min_ret.

  decide(T, buy(A, qty)) :- spike(A, T), flat(A, T).
  decide(T, sell(A, Q)) :- held(A, T, Q), lag(T, hold, T0), decided(T0, buy(A, _)).
}

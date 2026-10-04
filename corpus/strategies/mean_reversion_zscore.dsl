# Buy when the close sits more than `entry` standard deviations below its
# rolling mean; sell once the z-score recovers above `exit`.
strategy mean_reversion_zscore {
  env equities_1d
  uses features
  resolution @1d
  mode delta

  param n : Duration = 20d in 10d..60d
  param k : Count = 12
  param entry : Scalar = -2.0 in -4.0..-1.0
  param exit : Scalar = 0.0
  param qty : Quantity<Shares> = 100 shares

  rel oversold(-A: Equity, @T: Timestamp)
  oversold(A, T) :- universe(A, T), zscore(A, T, n, k, Z), Z < entry.

  rel recovered(-A: Equity, @T: Timestamp)
  recovered(A, T) :- universe(A, T), zscore(A, T, n, k, Z), Z > exit.

  decide(T, buy(A, qty)) :- oversold(A, T), flat(A, T).
  decide(T, sell(A, Q)) :- recovered(A, T), held(A, T, Q).
}

# Buy on a fast/slow moving-average cross up, sell on the cross down.
# `cross` is written positively: `not above(A, T0)` would be rejected by
# WF-5, since `above` depends on the incomplete primitive `close`.
strategy sma_crossover {
  env equities_1d
  uses features
  resolution @1d
  mode delta

  param fast : Duration = 20d in 10d..60d
  param slow : Duration = 50d in 30d..250d
  param fast_min : Count = 12
  param slow_min : Count = 30
  param qty : Quantity<Shares> = 100 shares

  rel above(-A: Equity, @T: Timestamp)
  above(A, T) :- universe(A, T), sma(A, T, fast, fast_min, F), sma(A, T, slow, slow_min, S), F > S.

  rel below(-A: Equity, @T: Timestamp)
  below(A, T) :- universe(A, T), sma(A, T, fast, fast_min, F), sma(A, T, slow, slow_min, S), F <= S.

  decide(T, buy(A, qty)) :- above(A, T), prev(T, T0), below(A, T0), flat(A, T).
  decide(T, sell(A, Q)) :- below(A, T), prev(T, T0), above(A, T0), held(A, T, Q).
}

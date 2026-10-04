# Momentum top-n while the benchmark trades above its trend; flat otherwise.
# WF-5's teeth: `selected` includes the benchmark condition, so it is not
# complete and `not selected(A, T)` is rejected; a missing benchmark close
# cannot liquidate the book. Liquidation negates `in_top`, a reduction.
strategy regime_momentum {
  env equities_1d
  uses features
  resolution @1d
  mode target

  param benchmark : Equity = "SPY"
  param trend : Duration = 200d in 100d..300d
  param trend_min : Count = 120
  param lookback : Duration = 6mo
  param skip : Duration = 1mo
  param min_adv : Notional<USD> = 1_000_000 USD
  param n : Count = 3

  rel regime_on(@T: Timestamp)
  regime_on(T) :- close(benchmark, T, P), sma(benchmark, T, trend, trend_min, M), P > M.

  rel regime_off(@T: Timestamp)
  regime_off(T) :- close(benchmark, T, P), sma(benchmark, T, trend, trend_min, M), P <= M.

  rel candidate(-A: Equity, @T: Timestamp, -M: Scalar)
  candidate(A, T, M) :- universe(A, T), mom_candidate(A, T, lookback, skip, min_adv, M).

  rel in_top(-A: Equity, @T: Timestamp)
  in_top(A, T) :- bar(T), top(n, candidate(A, T, M), by (M desc, A asc)).

  rel selected(-A: Equity, @T: Timestamp)
  selected(A, T) :- regime_on(T), in_top(A, T).

  rel n_selected(@T: Timestamp, -N: Count)
  n_selected(T, N) :- regime_on(T), N = count(A) over (in_top(A, T)), N > 0.

  decide(T, target_weight(A, W)) :- selected(A, T), n_selected(T, N), W = 1 / N.
  decide(T, target_weight(A, 0)) :- regime_on(T), held(A, T, _), not in_top(A, T).
  decide(T, target_weight(A, 0)) :- regime_off(T), held(A, T, _).
}
